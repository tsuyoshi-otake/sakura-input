//! Developer input-history viewing, export, and administration.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io;
use std::path::Path;
use std::time::Duration;

use sakura_engine::input_history::{
    clear_path, read_snapshot, InputHistoryRecord, InputHistorySnapshot, KeyHistoryRecord,
    ScopeClass, INPUT_HISTORY_FORMAT_VERSION,
};
use sakura_ipc::diagnostics::{record_timeout, TimeoutOperation};
use sakura_ipc::{Client, Endpoint, Fault, ServerTrustPolicy};
use sakura_proto::{Request, Response, PROTOCOL_VERSION};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND};
#[cfg(windows)]
use windows::Win32::Globalization::{NormalizationC, NormalizeString};

use crate::storage::atomic_write;

const ADMIN_CALL_BUDGET: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearRoute {
    LiveEngine,
    Offline { cleared_records: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushRoute {
    LiveEngine,
    Offline,
}

const MAX_REVIEW_CANDIDATES: usize = 100;
const MAX_REVIEW_TEXT_CHARS: usize = 96;
const MAX_REVIEW_TYPING_CHARS: usize = 128;
const MAX_OCCURRENCES_PER_SESSION_DAY: u8 = 3;
const MAX_OCCURRENCES_PER_PAIR: u32 = 32;
const MILLIS_PER_DAY: u64 = 86_400_000;
const REVIEW_PROVENANCE: &str = "local-opt-in-normal-commit-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewCandidate {
    pub case_id: String,
    pub family: String,
    pub input_mode: String,
    pub reading: String,
    pub typing: String,
    pub left_context: String,
    pub right_context: String,
    pub frequency_bucket: String,
    pub privacy_provenance: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MineStats {
    pub records_seen: usize,
    pub commit_records_seen: usize,
    pub accepted_commits: usize,
    pub excluded_non_normal: usize,
    pub excluded_private: usize,
    pub excluded_unreconstructable: usize,
    pub capped_occurrences: usize,
    pub ignored_tail_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MineReport {
    pub candidates: Vec<ReviewCandidate>,
    pub stats: MineStats,
    pub family_counts: BTreeMap<String, usize>,
}

#[derive(Debug, Clone)]
struct PairAggregate {
    reading: String,
    surface: String,
    count: u32,
    typist_counts: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SessionDay {
    session: u64,
    day: u64,
}

/// Mines only the already-decrypted snapshot in memory.
///
/// This boundary deliberately returns a review-only projection. Session IDs,
/// timestamps, process names, sequence numbers, surfaces, and raw records
/// never leave this function. The caller may persist the projection locally
/// for explicit human approval, but it is not a history export.
pub fn mine_snapshot(snapshot: &InputHistorySnapshot) -> MineReport {
    let mut stats = MineStats {
        records_seen: snapshot.records.len(),
        ignored_tail_bytes: snapshot.ignored_tail_bytes,
        ..MineStats::default()
    };
    let mut commits = Vec::new();
    let mut keys_by_session: BTreeMap<u64, Vec<&KeyHistoryRecord>> = BTreeMap::new();
    for record in &snapshot.records {
        match record {
            InputHistoryRecord::Commit(commit) => {
                stats.commit_records_seen += 1;
                commits.push(commit);
            }
            InputHistoryRecord::Key(key) => {
                keys_by_session.entry(key.session).or_default().push(key);
            }
            InputHistoryRecord::AiText(_) | InputHistoryRecord::Engine(_) => {}
        }
    }
    commits.sort_by_key(|record| (record.session, record.sequence));
    for keys in keys_by_session.values_mut() {
        keys.sort_by_key(|record| record.sequence);
    }

    let mut previous_commit = BTreeMap::<u64, u64>::new();
    let mut aggregates = BTreeMap::<(String, String), PairAggregate>::new();
    let mut session_day_counts = BTreeMap::<((String, String), SessionDay), u8>::new();
    for commit in commits {
        let lower_bound = previous_commit
            .insert(commit.session, commit.sequence)
            .unwrap_or(0);
        if commit.scope != ScopeClass::Normal
            || !safe_text(&commit.reading)
            || !safe_text(&commit.surface)
        {
            if commit.scope != ScopeClass::Normal {
                stats.excluded_non_normal += 1;
            } else {
                stats.excluded_private += 1;
            }
            continue;
        }
        let Some(reading) = normalize_and_bound(&commit.reading, MAX_REVIEW_TEXT_CHARS) else {
            stats.excluded_private += 1;
            continue;
        };
        let Some(surface) = normalize_and_bound(&commit.surface, MAX_REVIEW_TEXT_CHARS) else {
            stats.excluded_private += 1;
            continue;
        };
        let Some(typing) = reconstruct_typing(
            keys_by_session
                .get(&commit.session)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            lower_bound,
            commit.sequence,
            &reading,
        ) else {
            stats.excluded_unreconstructable += 1;
            continue;
        };
        let Some(typing) = normalize_and_bound(&typing, MAX_REVIEW_TYPING_CHARS) else {
            stats.excluded_private += 1;
            continue;
        };
        let pair = (reading.clone(), surface.clone());
        let session_day = SessionDay {
            session: commit.session,
            day: commit.timestamp_ms / MILLIS_PER_DAY,
        };
        let per_day = session_day_counts
            .entry((pair.clone(), session_day))
            .or_default();
        if *per_day >= MAX_OCCURRENCES_PER_SESSION_DAY {
            stats.capped_occurrences += 1;
            continue;
        }
        let aggregate = aggregates.entry(pair).or_insert_with(|| PairAggregate {
            reading,
            surface,
            count: 0,
            typist_counts: BTreeMap::new(),
        });
        if aggregate.count >= MAX_OCCURRENCES_PER_PAIR {
            stats.capped_occurrences += 1;
            continue;
        }
        *per_day += 1;
        aggregate.count += 1;
        *aggregate.typist_counts.entry(typing).or_default() += 1;
        stats.accepted_commits += 1;
    }

    let mut by_family = BTreeMap::<String, Vec<PairAggregate>>::new();
    for aggregate in aggregates.into_values() {
        by_family
            .entry(candidate_family(&aggregate.reading))
            .or_default()
            .push(aggregate);
    }
    for aggregates in by_family.values_mut() {
        aggregates.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| left.reading.cmp(&right.reading))
                .then_with(|| left.surface.cmp(&right.surface))
        });
    }

    let family_count = by_family.len().max(1);
    let family_quota = MAX_REVIEW_CANDIDATES.div_ceil(family_count);
    let mut selected = Vec::new();
    let mut selected_by_family = BTreeMap::<String, usize>::new();
    for (family, aggregates) in &by_family {
        for aggregate in aggregates.iter().take(family_quota) {
            if selected.len() == MAX_REVIEW_CANDIDATES {
                break;
            }
            selected.push((family.clone(), aggregate.clone()));
            *selected_by_family.entry(family.clone()).or_default() += 1;
        }
    }
    if selected.len() < MAX_REVIEW_CANDIDATES {
        let mut remainder = Vec::new();
        for (family, aggregates) in &by_family {
            let already = selected_by_family.get(family).copied().unwrap_or(0);
            remainder.extend(
                aggregates
                    .iter()
                    .skip(already)
                    .cloned()
                    .map(|aggregate| (family.clone(), aggregate)),
            );
        }
        remainder.sort_by(|(left_family, left), (right_family, right)| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| left_family.cmp(right_family))
                .then_with(|| left.reading.cmp(&right.reading))
                .then_with(|| left.surface.cmp(&right.surface))
        });
        selected.extend(
            remainder
                .into_iter()
                .take(MAX_REVIEW_CANDIDATES - selected.len()),
        );
    }

    let candidates = selected
        .into_iter()
        .map(|(family, aggregate)| {
            let (typing, _) = aggregate
                .typist_counts
                .into_iter()
                .max_by(|(left_typing, left_count), (right_typing, right_count)| {
                    left_count
                        .cmp(right_count)
                        .then_with(|| right_typing.cmp(left_typing))
                })
                .expect("accepted aggregate has a typing");
            ReviewCandidate {
                case_id: opaque_case_id(&aggregate.reading, &aggregate.surface),
                family,
                input_mode: candidate_input_mode(&typing).to_owned(),
                reading: aggregate.reading,
                typing,
                left_context: String::new(),
                right_context: String::new(),
                frequency_bucket: frequency_bucket(aggregate.count).to_owned(),
                privacy_provenance: REVIEW_PROVENANCE.to_owned(),
            }
        })
        .collect::<Vec<_>>();
    let mut family_counts = BTreeMap::new();
    for candidate in &candidates {
        *family_counts.entry(candidate.family.clone()).or_default() += 1;
    }
    MineReport {
        candidates,
        stats,
        family_counts,
    }
}

/// Renders only the bounded review projection. It intentionally does not
/// include the committed surface or any history identity/time metadata.
pub fn render_review_tsv(report: &MineReport) -> String {
    let mut output = String::from(
        "# sakura-history-review-format: 1\n\
         # generated-from: local-opt-in-normal-commits\n\
         case-id\tfamily\tinput-mode\treading\ttyping\tleft-context\tright-context\t\
frequency-bucket\tprivacy-provenance\n",
    );
    for candidate in &report.candidates {
        let _ = writeln!(
            output,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            escape_review_field(&candidate.case_id),
            escape_review_field(&candidate.family),
            escape_review_field(&candidate.input_mode),
            escape_review_field(&candidate.reading),
            escape_review_field(&candidate.typing),
            escape_review_field(&candidate.left_context),
            escape_review_field(&candidate.right_context),
            escape_review_field(&candidate.frequency_bucket),
            escape_review_field(&candidate.privacy_provenance),
        );
    }
    output
}

fn reconstruct_typing(
    keys: &[&KeyHistoryRecord],
    lower_bound: u64,
    upper_bound: u64,
    reading: &str,
) -> Option<String> {
    let mut typing = String::new();
    for key in keys
        .iter()
        .copied()
        .filter(|key| key.sequence > lower_bound && key.sequence < upper_bound)
    {
        if key.scope != ScopeClass::Normal
            || key.repeat
            || key.delete_before > 0
            || key.beep
            || key.modifiers != 0
        {
            return None;
        }
        if let Some(character) = key.character {
            if !is_typing_character(character) {
                return None;
            }
            typing.push(character);
        }
    }
    if typing.is_empty() {
        // A kana-layout record may not carry individual character metadata.
        // Reading is safe as a fallback only when it is already a bounded
        // kana sequence; arbitrary raw text is never invented here.
        if reading.chars().all(is_kana_character) {
            return Some(reading.to_owned());
        }
        return None;
    }
    Some(typing)
}

fn is_typing_character(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(character, '-' | '_' | '/' | '.' | ',' | '\'' | ' ')
        || is_kana_character(character)
}

fn is_kana_character(character: char) -> bool {
    matches!(
        character,
        '\u{3040}'..='\u{309f}' | '\u{30a0}'..='\u{30ff}' | '\u{31f0}'..='\u{31ff}'
    )
}

fn safe_text(value: &str) -> bool {
    if value.is_empty()
        || value.chars().count() > MAX_REVIEW_TEXT_CHARS
        || value.chars().any(|character| {
            character.is_control() || matches!(character, '\t' | '\r' | '\n' | '\u{0}')
        })
    {
        return false;
    }
    let lower = value.to_ascii_lowercase();
    if lower.contains("://")
        || lower.starts_with("www.")
        || (value.contains('@')
            && value
                .rsplit_once('@')
                .is_some_and(|(_, domain)| domain.contains('.') && !domain.starts_with('.')))
        || lower.starts_with("bearer ")
        || lower.starts_with("basic ")
        || lower.starts_with("sk-")
        || lower.starts_with("ghp_")
        || lower.starts_with("github_pat_")
        || lower.starts_with("xoxb-")
        || lower.starts_with("xoxp-")
        || looks_like_path(value)
        || looks_like_long_identifier(value)
    {
        return false;
    }
    true
}

fn looks_like_path(value: &str) -> bool {
    value.contains(":\\")
        || value.contains(":/")
        || value.starts_with("\\\\")
        || value.contains("/Users/")
        || value.contains("/home/")
}

fn looks_like_long_identifier(value: &str) -> bool {
    let chars = value.chars().collect::<Vec<_>>();
    if chars.len() < 24 || !chars.iter().all(|character| character.is_ascii()) {
        return false;
    }
    let has_lower = chars.iter().any(|character| character.is_ascii_lowercase());
    let has_upper = chars.iter().any(|character| character.is_ascii_uppercase());
    let has_digit = chars.iter().any(|character| character.is_ascii_digit());
    let has_punctuation = chars
        .iter()
        .any(|character| !character.is_ascii_alphanumeric());
    (has_lower && has_upper && has_digit) || (has_digit && has_punctuation && chars.len() >= 32)
}

fn normalize_and_bound(value: &str, max_chars: usize) -> Option<String> {
    let normalized = normalize_nfc(value)?;
    if normalized.is_empty()
        || normalized.chars().count() > max_chars
        || normalized.chars().any(|character| character.is_control())
    {
        return None;
    }
    Some(normalized)
}

#[cfg(windows)]
fn normalize_nfc(value: &str) -> Option<String> {
    let source = value.encode_utf16().collect::<Vec<_>>();
    // The source is a Rust UTF-8 string, so it cannot contain an unpaired
    // surrogate. NormalizeString returns the exact UTF-16 size required.
    // SAFETY: `source` is a valid UTF-16 slice and the optional destination
    // is null for this size query, as required by NormalizeString.
    let required = unsafe { NormalizeString(NormalizationC, &source, None) };
    if required <= 0 {
        return None;
    }
    let mut destination = vec![0u16; required as usize];
    // SAFETY: `source` is valid UTF-16 and `destination` has the size
    // returned by the preceding NormalizeString size query.
    let written = unsafe { NormalizeString(NormalizationC, &source, Some(&mut destination)) };
    if written <= 0 {
        return None;
    }
    String::from_utf16(&destination[..written as usize]).ok()
}

#[cfg(not(windows))]
fn normalize_nfc(value: &str) -> Option<String> {
    Some(value.to_owned())
}

fn candidate_family(reading: &str) -> String {
    let has_ascii = reading.chars().any(|character| character.is_ascii());
    let has_hiragana = reading
        .chars()
        .any(|character| matches!(character, '\u{3040}'..='\u{309f}'));
    let has_katakana = reading
        .chars()
        .any(|character| matches!(character, '\u{30a0}'..='\u{30ff}'));
    if has_ascii && (has_hiragana || has_katakana) {
        "mixed-romaji".to_owned()
    } else if has_katakana && !has_hiragana && !has_ascii {
        "katakana".to_owned()
    } else if has_ascii {
        "technical-terms".to_owned()
    } else {
        "normal-conversion".to_owned()
    }
}

fn candidate_input_mode(typing: &str) -> &'static str {
    if typing.is_ascii() {
        "romaji"
    } else {
        "kana"
    }
}

fn frequency_bucket(count: u32) -> &'static str {
    match count {
        0..=1 => "rare",
        2..=3 => "occasional",
        4..=7 => "frequent",
        _ => "very-frequent",
    }
}

fn opaque_case_id(reading: &str, surface: &str) -> String {
    let first = stable_hash(0xcbf2_9ce4_8422_2325, reading, surface);
    let second = stable_hash(0x8422_2325_cbf2_9ce4, surface, reading);
    format!("hist-{first:016x}{second:016x}")
}

fn stable_hash(seed: u64, first: &str, second: &str) -> u64 {
    let mut hash = seed;
    for byte in b"sakura-history-review-v1\0".iter().copied() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    for value in [first, second] {
        for byte in value.as_bytes().iter().copied() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn escape_review_field(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryStats {
    pub active: bool,
    pub dropped_events: u64,
    pub persistence_failures: u64,
    pub excluded_unclassified_events: u64,
    pub excluded_sensitive_events: u64,
    pub excluded_test_only_events: u64,
    pub ai_requests: u64,
    pub ai_attempts: u64,
    pub ai_input_tokens: u64,
    pub ai_output_tokens: u64,
    pub ai_cached_tokens: u64,
    pub live: bool,
}

pub fn view(path: &Path) -> io::Result<InputHistorySnapshot> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64;
    view_at(path, now_ms)
}

fn view_at(path: &Path, now_ms: u64) -> io::Result<InputHistorySnapshot> {
    match read_snapshot(path) {
        Ok(mut snapshot) => {
            snapshot.retain_current_records(now_ms);
            Ok(snapshot)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(InputHistorySnapshot {
            format_version: INPUT_HISTORY_FORMAT_VERSION,
            records: Vec::new(),
            ignored_tail_bytes: 0,
        }),
        Err(error) => Err(error),
    }
}

pub fn export(source: &Path, destination: &Path) -> io::Result<usize> {
    // A missing file means the developer-history service has never been
    // started for this user, so there cannot be queued records to flush. This
    // also keeps an empty export independent of whichever engine binary may
    // happen to be running while settings tests execute.
    if source.exists() {
        let _ = flush(source)?;
    }
    let snapshot = view(source)?;
    atomic_write(destination, snapshot.to_tsv().as_bytes())?;
    Ok(snapshot.records.len())
}

pub fn mine(source: &Path, destination: &Path) -> io::Result<MineReport> {
    // Mining is deliberately a read-only snapshot operation. The frame
    // scanner ignores an incomplete append, so a live engine cannot be
    // blocked by this developer-only analysis command.
    let snapshot = view(source)?;
    let report = mine_snapshot(&snapshot);
    atomic_write(destination, render_review_tsv(&report).as_bytes())?;
    Ok(report)
}

pub fn flush(_path: &Path) -> io::Result<FlushRoute> {
    let policy = installed_root_policy()?;
    let mut client =
        match Client::connect_endpoint_verified(Endpoint::Control, &policy, ADMIN_CALL_BUDGET) {
            Ok(client) => client,
            Err(error) if engine_is_definitely_absent(&error) => return Ok(FlushRoute::Offline),
            Err(error) => return Err(fault("connect to engine", error)),
        };
    handshake(&mut client)?;
    match client.call(&Request::FlushInputHistory, ADMIN_CALL_BUDGET) {
        Ok(Response::Ok) => Ok(FlushRoute::LiveEngine),
        Ok(Response::Error(code)) => Err(io::Error::other(format!(
            "engine could not flush input history: {code:?}"
        ))),
        Ok(response) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected input-history flush response: {response:?}"),
        )),
        Err(Fault::Timeout) => {
            let _ = record_timeout(TimeoutOperation::Administration);
            Err(fault("flush input history through engine", Fault::Timeout))
        }
        Err(error) => Err(fault("flush input history through engine", error)),
    }
}

pub fn stats(_path: &Path) -> io::Result<HistoryStats> {
    let policy = installed_root_policy()?;
    let mut client =
        match Client::connect_endpoint_verified(Endpoint::Control, &policy, ADMIN_CALL_BUDGET) {
            Ok(client) => client,
            Err(error) if engine_is_definitely_absent(&error) => {
                return Ok(HistoryStats {
                    active: false,
                    dropped_events: 0,
                    persistence_failures: 0,
                    excluded_unclassified_events: 0,
                    excluded_sensitive_events: 0,
                    excluded_test_only_events: 0,
                    ai_requests: 0,
                    ai_attempts: 0,
                    ai_input_tokens: 0,
                    ai_output_tokens: 0,
                    ai_cached_tokens: 0,
                    live: false,
                })
            }
            Err(error) => return Err(fault("connect to engine", error)),
        };
    handshake(&mut client)?;
    match client.call(&Request::InputHistoryStats, ADMIN_CALL_BUDGET) {
        Ok(Response::InputHistoryStats {
            active,
            dropped_events,
            persistence_failures,
            excluded_unclassified_events,
            excluded_sensitive_events,
            excluded_test_only_events,
            ai_requests,
            ai_attempts,
            ai_input_tokens,
            ai_output_tokens,
            ai_cached_tokens,
        }) => Ok(HistoryStats {
            active,
            dropped_events,
            persistence_failures,
            excluded_unclassified_events,
            excluded_sensitive_events,
            excluded_test_only_events,
            ai_requests,
            ai_attempts,
            ai_input_tokens,
            ai_output_tokens,
            ai_cached_tokens,
            live: true,
        }),
        Ok(Response::Error(code)) => Err(io::Error::other(format!(
            "engine could not read input-history stats: {code:?}"
        ))),
        Ok(response) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected input-history stats response: {response:?}"),
        )),
        Err(Fault::Timeout) => {
            let _ = record_timeout(TimeoutOperation::Administration);
            Err(fault(
                "read input-history stats through engine",
                Fault::Timeout,
            ))
        }
        Err(error) => Err(fault("read input-history stats through engine", error)),
    }
}

pub fn clear(path: &Path) -> io::Result<ClearRoute> {
    let policy = installed_root_policy()?;
    let mut client =
        match Client::connect_endpoint_verified(Endpoint::Control, &policy, ADMIN_CALL_BUDGET) {
            Ok(client) => client,
            Err(error) if engine_is_definitely_absent(&error) => return clear_offline(path),
            Err(error) => return Err(fault("connect to engine", error)),
        };
    handshake(&mut client)?;
    match client.call(&Request::ClearInputHistory, ADMIN_CALL_BUDGET) {
        Ok(Response::Ok) => Ok(ClearRoute::LiveEngine),
        Ok(Response::Error(code)) => Err(io::Error::other(format!(
            "engine could not clear input history: {code:?}"
        ))),
        Ok(response) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected input-history clear response: {response:?}"),
        )),
        Err(Fault::Timeout) => {
            let _ = record_timeout(TimeoutOperation::Administration);
            Err(fault("clear input history through engine", Fault::Timeout))
        }
        Err(error) => Err(fault("clear input history through engine", error)),
    }
}

pub fn clear_offline(path: &Path) -> io::Result<ClearRoute> {
    Ok(ClearRoute::Offline {
        cleared_records: clear_path(path)?,
    })
}

fn handshake(client: &mut Client) -> io::Result<()> {
    match client.call(
        &Request::Hello {
            client_version: PROTOCOL_VERSION,
        },
        ADMIN_CALL_BUDGET,
    ) {
        Ok(Response::Hello { server_version, .. }) if server_version == PROTOCOL_VERSION => Ok(()),
        Ok(Response::Error(code)) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("engine rejected settings handshake: {code:?}"),
        )),
        Ok(response) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected settings handshake response: {response:?}"),
        )),
        Err(Fault::Timeout) => {
            let _ = record_timeout(TimeoutOperation::Administration);
            Err(fault("negotiate with engine", Fault::Timeout))
        }
        Err(error) => Err(fault("negotiate with engine", error)),
    }
}

fn fault(action: &str, error: Fault) -> io::Error {
    let kind = match error {
        Fault::Timeout | Fault::DeadlineExpired => io::ErrorKind::TimedOut,
        Fault::Disconnected => io::ErrorKind::BrokenPipe,
        Fault::Protocol(_) | Fault::Desynchronized => io::ErrorKind::InvalidData,
        Fault::Encode(_) => io::ErrorKind::InvalidInput,
        Fault::UntrustedServer { .. } => io::ErrorKind::PermissionDenied,
        Fault::Os(_) => io::ErrorKind::Other,
    };
    io::Error::new(kind, format!("{action}: {error}"))
}

fn installed_root_policy() -> io::Result<ServerTrustPolicy> {
    let executable = std::env::current_exe()?;
    let root = executable
        .parent()
        .and_then(|release| release.parent())
        .and_then(|versions| versions.parent())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "settings executable is not versioned",
            )
        })?;
    Ok(ServerTrustPolicy::InstalledRoot(root.to_path_buf()))
}

fn engine_is_definitely_absent(error: &Fault) -> bool {
    let Fault::Os(error) = error else {
        return false;
    };
    let raw = error.code().0 as u32;
    let file_not_found = 0x8007_0000 | ERROR_FILE_NOT_FOUND.0;
    let path_not_found = 0x8007_0000 | ERROR_PATH_NOT_FOUND.0;
    raw == file_not_found || raw == path_not_found
}

#[cfg(test)]
mod tests {
    use super::*;
    use sakura_engine::input_history::{CommitHistoryRecord, InputHistoryRecord, KeyHistoryRecord};
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn temporary_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "sakura-settings-input-history-{}-{name}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn missing_history_views_and_exports_as_empty() {
        let directory = temporary_path("missing");
        let source = directory.join("missing.bin");
        let destination = directory.join("history.tsv");
        assert!(view(&source).expect("view").records.is_empty());
        assert_eq!(export(&source, &destination).expect("export"), 0);
        assert!(std::fs::read_to_string(destination)
            .expect("TSV")
            .contains("engine-package-version\tengine-release-label"));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn viewing_expires_records_without_mutating_raw_history() {
        struct Fixture(std::path::PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                if self.0.exists() {
                    std::fs::remove_file(&self.0).expect("remove owned history fixture");
                }
            }
        }
        let fixture = Fixture(temporary_path("retention-view"));
        let service = sakura_engine::input_history::InputHistoryService::open(&fixture.0)
            .expect("open isolated history");
        service.record_commit(1, ScopeClass::Normal, "synthetic", "synthetic", 0, 0);
        service.stop().expect("stop isolated history");
        let original = std::fs::read(&fixture.0).unwrap();
        let raw = read_snapshot(&fixture.0).unwrap();
        assert_eq!(raw.records.len(), 2);
        assert!(view_at(&fixture.0, u64::MAX).unwrap().records.is_empty());
        assert_eq!(view_at(&fixture.0, 0).unwrap(), raw);
        assert_eq!(view(&fixture.0).unwrap(), raw);
        assert_eq!(std::fs::read(&fixture.0).unwrap(), original);
    }

    fn key(
        sequence: u64,
        session: u64,
        character: Option<char>,
        scope: ScopeClass,
    ) -> InputHistoryRecord {
        InputHistoryRecord::Key(KeyHistoryRecord {
            sequence,
            timestamp_ms: 1_700_000_000_000,
            session,
            scope,
            key_code: 0,
            character,
            modifiers: 0,
            repeat: false,
            consumed: true,
            state_before: 0,
            state_after: 0,
            mode_before: 0,
            mode_after: 0,
            preedit_before: String::new(),
            preedit_after: String::new(),
            commit: String::new(),
            delete_before: 0,
            beep: false,
            action: String::new(),
            dropped_before: 0,
        })
    }

    fn commit(
        sequence: u64,
        session: u64,
        scope: ScopeClass,
        reading: &str,
        surface: &str,
    ) -> InputHistoryRecord {
        InputHistoryRecord::Commit(CommitHistoryRecord {
            sequence,
            timestamp_ms: 1_700_000_000_000,
            session,
            scope,
            reading: reading.to_owned(),
            surface: surface.to_owned(),
            left_context: 0,
            right_context: 0,
        })
    }

    #[test]
    fn miner_filters_private_scopes_and_reconstructs_typing() {
        let snapshot = InputHistorySnapshot {
            format_version: INPUT_HISTORY_FORMAT_VERSION,
            records: vec![
                key(1, 7, Some('k'), ScopeClass::Normal),
                key(2, 7, Some('y'), ScopeClass::Normal),
                key(3, 7, Some('o'), ScopeClass::Normal),
                key(4, 7, Some('u'), ScopeClass::Normal),
                commit(5, 7, ScopeClass::Normal, "きょう", "今日"),
                commit(
                    6,
                    7,
                    ScopeClass::Sensitive,
                    "https://example.invalid",
                    "URL",
                ),
                commit(7, 7, ScopeClass::Normal, "mail@example.invalid", "メール"),
                commit(8, 7, ScopeClass::Normal, "A9bC7dE8fG9hI0jK1lM2nO3p", "秘密"),
                commit(9, 7, ScopeClass::Unclassified, "ひみつ", "秘密"),
            ],
            ignored_tail_bytes: 0,
        };
        let report = mine_snapshot(&snapshot);
        assert_eq!(report.candidates.len(), 1);
        assert_eq!(report.candidates[0].reading, "きょう");
        assert_eq!(report.candidates[0].typing, "kyou");
        assert_eq!(report.candidates[0].input_mode, "romaji");
        assert!(report.candidates[0].case_id.starts_with("hist-"));
        assert_eq!(report.stats.accepted_commits, 1);
        assert_eq!(report.stats.excluded_non_normal, 2);
        assert_eq!(report.stats.excluded_private, 2);
    }

    #[test]
    fn miner_is_deterministic_and_applies_session_day_cap() {
        let mut records = Vec::new();
        for index in 0..10 {
            let key_sequence = index * 2 + 1;
            records.push(key(key_sequence, 11, Some('a'), ScopeClass::Normal));
            records.push(commit(key_sequence + 1, 11, ScopeClass::Normal, "a", "あ"));
        }
        let snapshot = InputHistorySnapshot {
            format_version: INPUT_HISTORY_FORMAT_VERSION,
            records,
            ignored_tail_bytes: 0,
        };
        let mut reversed = snapshot.records.clone();
        reversed.reverse();
        let reversed_snapshot = InputHistorySnapshot {
            format_version: INPUT_HISTORY_FORMAT_VERSION,
            records: reversed,
            ignored_tail_bytes: 0,
        };
        let report = mine_snapshot(&snapshot);
        let reversed_report = mine_snapshot(&reversed_snapshot);
        assert_eq!(report, reversed_report);
        assert_eq!(report.candidates.len(), 1);
        assert_eq!(report.stats.accepted_commits, 3);
        assert_eq!(report.stats.capped_occurrences, 7);
        assert_eq!(report.candidates[0].frequency_bucket, "occasional");
        assert!(!render_review_tsv(&report).contains("session"));
    }

    #[test]
    fn miner_normalizes_nfc_before_issuing_opaque_id() {
        let snapshot = InputHistorySnapshot {
            format_version: INPUT_HISTORY_FORMAT_VERSION,
            records: vec![
                key(1, 12, Some('k'), ScopeClass::Normal),
                commit(2, 12, ScopeClass::Normal, "か\u{3099}", "が"),
            ],
            ignored_tail_bytes: 0,
        };
        let report = mine_snapshot(&snapshot);
        assert_eq!(report.candidates.len(), 1);
        assert_eq!(report.candidates[0].reading, "が");
    }

    #[test]
    fn miner_caps_review_output_at_one_hundred_cases() {
        let mut records = Vec::new();
        for index in 0..120 {
            let session = index + 1;
            let key_sequence = index * 2 + 1;
            records.push(key(key_sequence, session, Some('a'), ScopeClass::Normal));
            records.push(commit(
                key_sequence + 1,
                session,
                ScopeClass::Normal,
                &format!("term{index}"),
                &format!("表{index}"),
            ));
        }
        let report = mine_snapshot(&InputHistorySnapshot {
            format_version: INPUT_HISTORY_FORMAT_VERSION,
            records,
            ignored_tail_bytes: 0,
        });
        assert_eq!(report.candidates.len(), MAX_REVIEW_CANDIDATES);
        assert_eq!(report.family_counts.get("technical-terms"), Some(&100));
        assert_eq!(
            report
                .candidates
                .iter()
                .map(|candidate| candidate.case_id.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            100
        );
    }
}
