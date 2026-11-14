//! Replays a developer-history export through the committed-context
//! conversion path and reports counts only (#471 phase 1).
//!
//! The input file lives outside the repository (for example an export made
//! with `sakura_settings.exe history export`). Only commits that were real
//! conversions are measured: a `commit` record in `normal` scope counts as a
//! conversion when the most recent `key` record in the same session has
//! `action = convert` — the convert key itself committed the candidate.
//! Commits entered as typed (no conversion key) are not a ranking question.
//!
//! For a measured commit the previous `normal` commit in the same session is
//! the clue — the same carry the production path gives the next conversion —
//! and the record's own reading is converted with `initial_right_id` and the
//! cross-commit bridge. Only counts leave this module: readings, surfaces and
//! sessions are never returned, printed, or stored anywhere.

use std::collections::BTreeMap;

use sakura_core::{ConversionInput, ConversionOptions};
use sakura_engine::dictionary::ConversionService;

use crate::measure::commit_carry;

/// The four counters each breakdown reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Breakdown {
    /// Commits selected for measurement.
    pub target: usize,
    /// First candidate equals the committed surface.
    pub top_match: usize,
    /// First candidate differs from the committed surface.
    pub top_mismatch: usize,
    /// Top candidate and committed surface share the non-kanji character
    /// sequence and both contain kanji — a same-okurigana homophone swap.
    pub homophone_swap: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplayCounts {
    /// `commit` records in a normal scope.
    pub commits: usize,
    /// Commits whose preceding key in the session has `action = convert` —
    /// the only commits this replay measures.
    pub target: usize,
    /// First candidate equals the committed surface.
    pub top_match: usize,
    /// First candidate differs from the committed surface.
    pub top_mismatch: usize,
    /// Of the mismatches: committed surface and first candidate share the
    /// non-kanji character sequence and both contain kanji (#474's strict
    /// homophone-swap shape; being merely present in the candidate list does
    /// not count).
    pub homophone_swap: usize,
    /// Targets skipped: no preceding commit (nothing to carry) or the
    /// conversion call failed.
    pub skipped: usize,
    /// The same four counters restricted to readings of four characters or
    /// fewer — short readings dominate the homophone risk.
    pub short: Breakdown,
}

/// CJK ideographs and the iteration mark — the "kanji" of the swap shape.
fn is_kanji(c: char) -> bool {
    matches!(
        c,
        '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}' | '々'
    )
}

/// `first`/`committed` are a homophone swap when they differ, both contain
/// kanji, and their non-kanji characters (okurigana shape) are identical.
fn is_homophone_swap(first: &str, committed: &str) -> bool {
    first != committed
        && first.chars().any(is_kanji)
        && committed.chars().any(is_kanji)
        && first
            .chars()
            .filter(|c| !is_kanji(*c))
            .eq(committed.chars().filter(|c| !is_kanji(*c)))
}

/// Column names the export header is expected to carry. Extra or reordered
/// columns are tolerated; a missing one is an error.
fn column_indexes(header: &str) -> Result<(usize, usize, usize, usize, usize, usize), String> {
    let names: Vec<&str> = header.split('\t').collect();
    let find = |name: &str| {
        names
            .iter()
            .position(|column| *column == name)
            .ok_or_else(|| format!("history export has no '{name}' column"))
    };
    Ok((
        find("kind")?,
        find("session")?,
        find("scope")?,
        find("action")?,
        find("reading")?,
        find("surface")?,
    ))
}

/// Replays `source`, a UTF-8 history export TSV. `key` rows feed the action
/// tracker; `commit` rows in `normal` scope feed the clue chain and — when
/// they follow a `convert` key — the measurement itself.
pub fn replay_history(service: &ConversionService, source: &str) -> Result<ReplayCounts, String> {
    let mut counts = ReplayCounts::default();
    let mut header_seen = false;
    let mut indexes = (0, 0, 0, 0, 0, 0);
    // Per-session preceding commit: (reading, surface), and the most recent
    // key action. String values stay in memory only; nothing below this
    // function observes them.
    let mut last: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut last_action: BTreeMap<String, String> = BTreeMap::new();
    for (number, line) in source.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !header_seen {
            indexes = column_indexes(line)
                .map_err(|error| format!("history export:{}: {error}", number + 1))?;
            header_seen = true;
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let (kind, session, scope, action, reading, surface) = indexes;
        let width = [kind, session, scope, action, reading, surface]
            .iter()
            .max()
            .copied()
            .unwrap_or(0)
            + 1;
        if fields.len() < width {
            return Err(format!(
                "history export:{}: expected at least {width} columns",
                number + 1
            ));
        }
        if fields[kind] == "key" {
            last_action.insert(fields[session].to_owned(), fields[action].to_owned());
            continue;
        }
        if fields[kind] != "commit" || fields[scope] != "normal" {
            continue;
        }
        counts.commits += 1;
        let converted = last_action
            .get(fields[session])
            .is_some_and(|name| name == "convert");
        let clue = last.insert(
            fields[session].to_owned(),
            (fields[reading].to_owned(), fields[surface].to_owned()),
        );
        if !converted {
            continue;
        }
        counts.target += 1;
        let short = fields[reading].chars().count() <= 4;
        if short {
            counts.short.target += 1;
        }
        let Some((clue_reading, clue_surface)) = clue else {
            counts.skipped += 1;
            continue;
        };
        // An Err means the previous commit's surface is not reproducible (a
        // generated path or an erased/undone commit): production carries
        // nothing, so replay it as a no-bridge conversion rather than
        // dropping it.
        let (initial_right_id, bridge) =
            commit_carry(service, &clue_reading, &clue_surface).unwrap_or_default();
        let outcome = crate::measure::with_service(|| {
            service.with_conversion_input_bridge_hints(
                ConversionInput::ordinary(fields[reading]),
                ConversionOptions {
                    initial_right_id,
                    ..ConversionOptions::default()
                },
                &[],
                bridge,
                |candidates, _| {
                    candidates
                        .first()
                        .map(|candidate| candidate.text().to_owned())
                },
            )
        });
        let Ok(Some(top)) = outcome else {
            counts.skipped += 1;
            continue;
        };
        if top == fields[surface] {
            counts.top_match += 1;
            if short {
                counts.short.top_match += 1;
            }
        } else {
            counts.top_mismatch += 1;
            let swap = is_homophone_swap(&top, fields[surface]);
            if swap {
                counts.homophone_swap += 1;
            }
            if short {
                counts.short.top_mismatch += 1;
                if swap {
                    counts.short.homophone_swap += 1;
                }
            }
        }
    }
    if !header_seen {
        return Err("history export: no header line".to_owned());
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exported TSV's column order may drift; parsing must follow names.
    #[test]
    fn column_indexes_follow_the_header_names() {
        let header = "kind\tsequence\tsession\tscope\tkey-code\taction\treading\tsurface";
        let (kind, session, scope, action, reading, surface) =
            column_indexes(header).expect("columns");
        assert_eq!(
            (kind, session, scope, action, reading, surface),
            (0, 2, 3, 5, 6, 7)
        );
        assert!(column_indexes("kind\tsession").is_err());
    }

    #[test]
    fn homophone_swap_needs_same_okurigana_and_kanji_on_both_sides() {
        // Same non-kanji shape, different kanji: a swap.
        assert!(is_homophone_swap("生む", "産む"));
        assert!(is_homophone_swap("生まない", "産まない"));
        // Different okurigana: not a swap.
        assert!(!is_homophone_swap("生む", "産み"));
        assert!(!is_homophone_swap("生まない", "産む"));
        // No kanji on one side: not a swap.
        assert!(!is_homophone_swap("はい", "入"));
        assert!(!is_homophone_swap("入り", "はいり"));
        // Equal surfaces are a match, not a swap.
        assert!(!is_homophone_swap("生む", "生む"));
        // Kanji outside the ideograph ranges still count (々).
        assert!(is_homophone_swap("時々", "刻々"));
    }
}
