//! Replays a developer-history export through the committed-context
//! conversion path and reports counts only (#471 phase 1).
//!
//! The input file lives outside the repository (for example an export made
//! with `sakura_settings.exe history export`). For every `commit` record in a
//! `normal` scope the previous commit in the same session is the clue — the
//! same carry the production path gives the next conversion — and the record's
//! own reading is converted with `initial_right_id` and the cross-commit
//! bridge. Only counts leave this module: readings, surfaces and sessions are
//! never returned, printed, or stored anywhere.

use std::collections::BTreeMap;

use sakura_core::{ConversionInput, ConversionOptions};
use sakura_engine::dictionary::ConversionService;

use crate::measure::commit_carry;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplayCounts {
    /// `commit` records in a normal scope.
    pub commits: usize,
    /// Commits that could be replayed (preceding commit in the session,
    /// clue conversion produced the committed surface).
    pub replayed: usize,
    /// First candidate equals the committed surface.
    pub top_match: usize,
    /// First candidate differs from the committed surface.
    pub top_mismatch: usize,
    /// Committed surface was among the candidates but not first — the
    /// homophone-swap count.
    pub homophone_swap: usize,
    /// Commits skipped: no preceding commit, or the clue surface was not a
    /// candidate (generated/undo paths produce no carry).
    pub skipped: usize,
}

/// Column names the export header is expected to carry. Extra or reordered
/// columns are tolerated; a missing one is an error.
fn column_indexes(header: &str) -> Result<(usize, usize, usize, usize, usize), String> {
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
        find("reading")?,
        find("surface")?,
    ))
}

/// Replays `source`, a UTF-8 history export TSV. `commit` rows in `normal`
/// scope are the only input; `key`/`diag` rows and sensitive/unclassified
/// commits are ignored by construction.
pub fn replay_history(service: &ConversionService, source: &str) -> Result<ReplayCounts, String> {
    let mut counts = ReplayCounts::default();
    let mut header_seen = false;
    let mut indexes = (0, 0, 0, 0, 0);
    // Per-session preceding commit: (reading, surface). String keys stay in
    // memory only; nothing below this function observes them.
    let mut last: BTreeMap<String, (String, String)> = BTreeMap::new();
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
        let (kind, session, scope, reading, surface) = indexes;
        let width = [kind, session, scope, reading, surface]
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
        if fields[kind] != "commit" || fields[scope] != "normal" {
            continue;
        }
        counts.commits += 1;
        let clue = last.insert(
            fields[session].to_owned(),
            (fields[reading].to_owned(), fields[surface].to_owned()),
        );
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
                    let top = candidates
                        .first()
                        .map(|candidate| candidate.text() == fields[surface]);
                    let in_list = candidates
                        .iter()
                        .any(|candidate| candidate.text() == fields[surface]);
                    (top, in_list)
                },
            )
        });
        let Ok((top, in_list)) = outcome else {
            counts.skipped += 1;
            continue;
        };
        counts.replayed += 1;
        match top {
            Some(true) => counts.top_match += 1,
            Some(false) => {
                counts.top_mismatch += 1;
                if in_list {
                    counts.homophone_swap += 1;
                }
            }
            None => counts.skipped += 1,
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
        let header = "kind\tsequence\tsession\tscope\tkey-code\treading\tsurface";
        let (kind, session, scope, reading, surface) = column_indexes(header).expect("columns");
        assert_eq!((kind, session, scope, reading, surface), (0, 2, 3, 5, 6));
        assert!(column_indexes("kind\tsession").is_err());
    }
}
