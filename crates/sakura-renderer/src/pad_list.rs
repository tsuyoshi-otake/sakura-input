//! What the memo list shows, as plain values.
//!
//! Which memos are visible, in what order, and what each row reads as are all
//! derived here from the stored document. The window paints what this
//! produces and owns no ordering rule of its own, so "why is this memo here,
//! and why here" has exactly one place to answer it — and that place is
//! testable without an HWND.

use std::collections::HashMap;

use crate::pad_storage::{PadDocument, PadMemo, PadSort};

/// A row is one title line over one preview line.
pub(crate) const ROW_HEIGHT_96: i32 = 48;

/// The rail down the selected row, at 96 DPI.
///
/// Wider than the candidate popup's, which marks one line of a list read at a
/// glance. This marks which memo the whole right-hand pane is showing, and it
/// is the only thing on screen that says so.
pub(crate) const ROW_RAIL_96: i32 = 3;

/// A memo with no title still has to be findable in the list.
pub(crate) const UNTITLED: &str = "無題";

/// A protected memo without an explicit unlocked projection reveals no content.
pub(crate) const PROTECTED_MEMO: &str = "保護されたメモ";

/// Plaintext held by the window's unlock state, never by `PadDocument`.
#[derive(Clone, Copy)]
pub(crate) struct UnlockedMemo<'a> {
    pub(crate) title: &'a str,
    pub(crate) body: &'a str,
}

/// The content and timestamp a row is permitted to show.
pub(crate) enum MemoProjection<'a> {
    Readable {
        title: &'a str,
        body: &'a str,
        updated_ms: u64,
    },
    Locked,
}

impl MemoProjection<'_> {
    pub(crate) fn title(&self) -> &str {
        match self {
            Self::Readable { title, .. } if title.trim().is_empty() => UNTITLED,
            Self::Readable { title, .. } => title,
            Self::Locked => PROTECTED_MEMO,
        }
    }

    pub(crate) fn preview(&self) -> String {
        match self {
            Self::Readable { body, .. } => preview_body(body),
            Self::Locked => PROTECTED_MEMO.to_owned(),
        }
    }

    pub(crate) fn updated_ms(&self) -> Option<u64> {
        match self {
            Self::Readable { updated_ms, .. } => Some(*updated_ms),
            Self::Locked => None,
        }
    }

    fn searchable(&self, needle: &str) -> bool {
        match self {
            Self::Readable { title, body, .. } => {
                fold(title).contains(needle) || fold(body).contains(needle)
            }
            Self::Locked => false,
        }
    }

    fn is_locked(&self) -> bool {
        matches!(self, Self::Locked)
    }
}

/// The caller must supply decrypted text for a protected memo by ID. A plain
/// memo always reads its own document content, even if the map contains an ID.
pub(crate) fn projection<'a>(
    memo: &'a PadMemo,
    unlocked: Option<UnlockedMemo<'a>>,
) -> MemoProjection<'a> {
    if let Some((title, body)) = memo.plain_content() {
        MemoProjection::Readable {
            title,
            body,
            updated_ms: memo.updated_ms,
        }
    } else if let Some(UnlockedMemo { title, body }) = unlocked {
        MemoProjection::Readable {
            title,
            body,
            updated_ms: memo.updated_ms,
        }
    } else {
        MemoProjection::Locked
    }
}

/// A preview line is clipped by the row long before this, but the string
/// itself stays bounded so a 65,536-unit body never reaches `DrawTextW`.
const PREVIEW_CHARS: usize = 120;

/// The live memo ids the list shows, filtered by `query` and ordered by the
/// document's own sort.
///
/// Tombstones are never rows: a deleted memo is retained only so the deletion
/// can be published, and showing it back to the user would read as the delete
/// having failed.
pub(crate) fn rows(document: &PadDocument, query: &str) -> Vec<u64> {
    rows_with_unlocked(document, query, &HashMap::new())
}

/// Search and sort with only the currently unlocked protected content visible.
/// Locked rows have no searchable text or sortable title/time. They follow
/// readable rows in stable ID order in every sort mode.
pub(crate) fn rows_with_unlocked(
    document: &PadDocument,
    query: &str,
    unlocked: &HashMap<u64, UnlockedMemo<'_>>,
) -> Vec<u64> {
    let needle = fold(query);
    let mut visible: Vec<&PadMemo> = document
        .live()
        .filter(|memo| {
            needle.is_empty()
                || projection(memo, unlocked.get(&memo.id).copied()).searchable(&needle)
        })
        .collect();
    visible.sort_by(|left, right| order(document.sort, left, right, unlocked));
    visible.iter().map(|memo| memo.id).collect()
}

/// The comparison the search box and the sort control share.
///
/// Every order is total: `id` breaks every tie, so the list cannot reshuffle
/// two same-second memos between two repaints.
fn order(
    sort: PadSort,
    left: &PadMemo,
    right: &PadMemo,
    unlocked: &HashMap<u64, UnlockedMemo<'_>>,
) -> std::cmp::Ordering {
    let left_view = projection(left, unlocked.get(&left.id).copied());
    let right_view = projection(right, unlocked.get(&right.id).copied());
    match (left_view.is_locked(), right_view.is_locked()) {
        (true, true) => return left.id.cmp(&right.id),
        (true, false) => return std::cmp::Ordering::Greater,
        (false, true) => return std::cmp::Ordering::Less,
        (false, false) => {}
    }
    match sort {
        // Newest first, which is where the memo just edited is looked for.
        PadSort::Updated => right
            .updated_ms
            .cmp(&left.updated_ms)
            .then(left.id.cmp(&right.id)),
        PadSort::Created => right
            .created_ms
            .cmp(&left.created_ms)
            .then(left.id.cmp(&right.id)),
        // Code-point order, not a locale collation. It is stable, needs no
        // table, and it is honest: this is "名前順", not "五十音順".
        PadSort::Title => left_view
            .title()
            .cmp(right_view.title())
            .then(left.id.cmp(&right.id)),
    }
}

/// Case-folds for search. ASCII only: it makes `README` find `readme` without
/// claiming a Unicode collation the product does not have.
fn fold(value: &str) -> String {
    value.to_lowercase()
}

fn preview_body(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| {
            let mut preview: String = line.chars().take(PREVIEW_CHARS).collect();
            if line.chars().nth(PREVIEW_CHARS).is_some() {
                preview.push('…');
            }
            preview
        })
        .unwrap_or_default()
}

/// A local wall-clock reading, in the fields a row actually prints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CalendarTime {
    pub(crate) year: i64,
    pub(crate) month: u32,
    pub(crate) day: u32,
    pub(crate) hour: u32,
    pub(crate) minute: u32,
}

/// 1601-01-01, the FILETIME epoch, expressed in days before 1970-01-01.
const FILETIME_EPOCH_DAYS_BEFORE_UNIX: i64 = 134_774;
const TICKS_PER_SECOND: u64 = 10_000_000;
const SECONDS_PER_DAY: i64 = 86_400;

/// Converts a stored Unix millisecond stamp to the user's local wall clock.
///
/// Returns `None` for the zero stamp, which is what a migrated version 1 memo
/// carries: it records "this time is not known", and inventing 1970 for it
/// would be worse than saying so.
pub(crate) fn local_time(unix_ms: u64) -> Option<CalendarTime> {
    if unix_ms == 0 {
        return None;
    }
    let ticks = unix_ms
        .checked_add(11_644_473_600_000)?
        .checked_mul(10_000)?;
    let utc = windows::Win32::Foundation::FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut local = windows::Win32::Foundation::FILETIME::default();
    // SAFETY: both structures are live, initialized, and sized by their type.
    unsafe {
        windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime(&utc, &mut local).ok()?;
    }
    Some(civil(
        u64::from(local.dwHighDateTime) << 32 | u64::from(local.dwLowDateTime),
    ))
}

/// Splits FILETIME ticks into calendar fields.
///
/// Pure integer arithmetic, so every boundary the display cares about — a day
/// rollover, a year rollover, a leap day — is an executable test rather than
/// something only a machine with the right clock can show.
pub(crate) fn civil(ticks: u64) -> CalendarTime {
    let seconds = (ticks / TICKS_PER_SECOND) as i64;
    let days = seconds.div_euclid(SECONDS_PER_DAY) - FILETIME_EPOCH_DAYS_BEFORE_UNIX;
    let second_of_day = seconds.rem_euclid(SECONDS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    CalendarTime {
        year,
        month,
        day,
        hour: (second_of_day / 3_600) as u32,
        minute: ((second_of_day % 3_600) / 60) as u32,
    }
}

/// Days since 1970-01-01 to a proleptic Gregorian date.
///
/// The era-based form: it needs no leap-year table and no month table, and it
/// is correct for every day this product can store.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_position + 2) / 5 + 1) as u32;
    let month = (month_position + if month_position < 10 { 3 } else { -9 }) as u32;
    (year + i64::from(month <= 2), month, day)
}

/// How a memo's time reads next to the memo.
///
/// Today is a clock reading, because that is what distinguishes two edits made
/// this afternoon. Anything older is a date, because the hour of a memo from
/// March is not what the user is trying to tell apart.
pub(crate) fn format_time(now: Option<CalendarTime>, then: Option<CalendarTime>) -> String {
    let Some(then) = then else {
        // A migrated version 1 memo, whose stamp was never recorded.
        return "—".to_owned();
    };
    match now {
        Some(now) if now.year == then.year && now.month == then.month && now.day == then.day => {
            format!("{}:{:02}", then.hour, then.minute)
        }
        Some(now) if now.year == then.year => format!("{}/{}", then.month, then.day),
        _ => format!("{}/{}/{}", then.year, then.month, then.day),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pad_storage::PadMemo;

    fn document(sort: PadSort, memos: Vec<PadMemo>) -> PadDocument {
        PadDocument {
            document_id: [0; 16],
            generation: 1,
            sort,
            memos,
        }
    }

    fn memo(id: u64, title: &str, body: &str, created: u64, updated: u64) -> PadMemo {
        let mut memo = PadMemo::new(id, title, body, created);
        memo.updated_ms = updated;
        memo
    }

    fn protected(id: u64, title: &str, body: &str, created: u64, updated: u64) -> PadMemo {
        let mut memo = memo(id, title, body, created, updated);
        memo.protect_with_envelope(b"opaque-ciphertext".to_vec())
            .unwrap();
        memo
    }

    #[test]
    fn every_sort_is_total_and_puts_the_newest_first() {
        let memos = vec![
            memo(1, "beta", "", 300, 100),
            memo(2, "alpha", "", 100, 300),
            memo(3, "gamma", "", 200, 200),
        ];
        assert_eq!(
            rows(&document(PadSort::Updated, memos.clone()), ""),
            [2, 3, 1]
        );
        assert_eq!(
            rows(&document(PadSort::Created, memos.clone()), ""),
            [1, 3, 2]
        );
        assert_eq!(rows(&document(PadSort::Title, memos), ""), [2, 1, 3]);
    }

    /// Two memos saved in the same second must not swap places between two
    /// repaints; the list would look like it was rewriting itself.
    #[test]
    fn ties_are_broken_by_identity_so_the_order_never_flickers() {
        for sort in [PadSort::Updated, PadSort::Created, PadSort::Title] {
            let same = vec![
                memo(7, "same", "", 500, 500),
                memo(3, "same", "", 500, 500),
                memo(5, "same", "", 500, 500),
            ];
            assert_eq!(rows(&document(sort, same), ""), [3, 5, 7], "{sort:?}");
        }
    }

    #[test]
    fn search_reads_title_and_body_and_ignores_ascii_case() {
        let memos = vec![
            memo(1, "README", "", 1, 1),
            memo(2, "買い物リスト", "牛乳 と パン", 2, 2),
            memo(3, "無関係", "", 3, 3),
        ];
        let document = document(PadSort::Created, memos);
        assert_eq!(rows(&document, "readme"), [1]);
        assert_eq!(rows(&document, "パン"), [2]);
        assert_eq!(rows(&document, ""), [3, 2, 1]);
        assert!(rows(&document, "見つからない").is_empty());
    }

    /// A deleted memo is retained only so the deletion can be published. A row
    /// for it would read as the delete having silently failed.
    #[test]
    fn tombstones_are_never_rows() {
        let mut retired = memo(2, "消した", "本文", 2, 2);
        retired.retire(9);
        let document = document(PadSort::Updated, vec![memo(1, "残る", "", 1, 1), retired]);
        assert_eq!(rows(&document, ""), [1]);
        assert!(rows(&document, "消した").is_empty());
    }

    #[test]
    fn a_row_always_has_a_hittable_title_and_a_single_line_preview() {
        let untitled = memo(1, "   ", "\n\n  最初の行  \n二行目", 1, 1);
        assert_eq!(projection(&untitled, None).title(), UNTITLED);
        assert_eq!(projection(&untitled, None).preview(), "最初の行");
        let empty = memo(2, "題", "", 1, 1);
        assert_eq!(projection(&empty, None).title(), "題");
        assert_eq!(projection(&empty, None).preview(), "");
    }

    #[test]
    fn a_long_first_line_is_bounded_before_it_reaches_gdi() {
        let body = "あ".repeat(PREVIEW_CHARS * 4);
        let long = memo(1, "題", &body, 1, 1);
        let preview = projection(&long, None).preview();
        assert_eq!(preview.chars().count(), PREVIEW_CHARS + 1);
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn locked_content_has_constant_projection_and_never_answers_a_search() {
        let secret = protected(2, "hidden-title", "hidden-body", 900, 900);
        let plain = memo(1, "visible", "ordinary", 1, 1);
        let document = document(PadSort::Updated, vec![secret.clone(), plain]);

        let locked = projection(&secret, None);
        assert_eq!(locked.title(), PROTECTED_MEMO);
        assert_eq!(locked.preview(), PROTECTED_MEMO);
        assert_eq!(locked.updated_ms(), None);
        assert_eq!(projection(&secret, None).title(), PROTECTED_MEMO);
        assert_eq!(projection(&secret, None).preview(), PROTECTED_MEMO);
        for query in [
            "hidden-title",
            "hidden-body",
            "opaque-ciphertext",
            PROTECTED_MEMO,
        ] {
            assert!(rows(&document, query).is_empty(), "{query}");
        }
        assert_eq!(rows(&document, ""), [1, 2]);
    }

    #[test]
    fn explicit_unlock_controls_search_and_sort_without_changing_document() {
        let secret = protected(2, "persisted-secret", "persisted-body", 900, 900);
        let plain = memo(1, "beta", "ordinary", 1, 1);
        let other_locked = protected(3, "another-secret", "another-body", 999, 999);
        let unlocked = HashMap::from([(
            2,
            UnlockedMemo {
                title: "alpha",
                body: "decrypted-only",
            },
        )]);
        let title_document = document(
            PadSort::Title,
            vec![other_locked.clone(), secret.clone(), plain.clone()],
        );
        assert_eq!(
            rows_with_unlocked(&title_document, "", &unlocked),
            [2, 1, 3]
        );
        assert_eq!(
            rows_with_unlocked(&title_document, "DECRYPTED", &unlocked),
            [2]
        );
        assert!(rows_with_unlocked(&title_document, "persisted-secret", &unlocked).is_empty());
        assert_eq!(
            projection(&secret, unlocked.get(&2).copied()).title(),
            "alpha"
        );
        assert_eq!(
            projection(&secret, unlocked.get(&2).copied()).updated_ms(),
            Some(900)
        );
        assert!(secret.plain_content().is_none());

        let updated_document = document(PadSort::Updated, vec![other_locked, plain, secret]);
        assert_eq!(
            rows_with_unlocked(&updated_document, "", &unlocked),
            [2, 1, 3]
        );
        assert_eq!(rows(&updated_document, ""), [1, 2, 3]);
    }

    #[test]
    fn calendar_fields_survive_epochs_leap_days_and_year_ends() {
        // 1970-01-01T00:00:00Z, expressed in FILETIME ticks.
        let unix_epoch = (FILETIME_EPOCH_DAYS_BEFORE_UNIX as u64) * 86_400 * TICKS_PER_SECOND;
        assert_eq!(
            civil(unix_epoch),
            CalendarTime {
                year: 1970,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0
            }
        );
        let leap_day = unix_epoch + ((11_016 * 86_400 + 23 * 3_600 + 59 * 60) * TICKS_PER_SECOND);
        assert_eq!(
            civil(leap_day),
            CalendarTime {
                year: 2000,
                month: 2,
                day: 29,
                hour: 23,
                minute: 59
            }
        );
        let year_end = unix_epoch + ((20_453 * 86_400 + 10 * 3_600 + 27 * 60) * TICKS_PER_SECOND);
        assert_eq!(
            civil(year_end),
            CalendarTime {
                year: 2025,
                month: 12,
                day: 31,
                hour: 10,
                minute: 27
            }
        );
    }

    #[test]
    fn the_time_a_row_shows_narrows_as_the_memo_gets_closer_to_now() {
        let now = CalendarTime {
            year: 2026,
            month: 8,
            day: 25,
            hour: 18,
            minute: 4,
        };
        let today = CalendarTime {
            hour: 10,
            minute: 27,
            ..now
        };
        assert_eq!(format_time(Some(now), Some(today)), "10:27");
        assert_eq!(
            format_time(Some(now), Some(CalendarTime { day: 24, ..today })),
            "8/24"
        );
        assert_eq!(
            format_time(
                Some(now),
                Some(CalendarTime {
                    year: 2025,
                    ..today
                })
            ),
            "2025/8/25"
        );
    }

    /// A migrated version 1 memo has no recorded time. Printing 1970 for it
    /// would be a confident answer to a question the file never answered.
    #[test]
    fn an_unknown_time_says_so_instead_of_inventing_one() {
        let now = CalendarTime {
            year: 2026,
            month: 8,
            day: 25,
            hour: 18,
            minute: 4,
        };
        assert_eq!(format_time(Some(now), None), "—");
        assert_eq!(local_time(0), None);
    }
}
