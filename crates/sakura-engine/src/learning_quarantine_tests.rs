//! #360 A3: mid-file corruption is quarantined as `log.corrupt-*` before the
//! log is truncated to its valid prefix (K1, K2), torn tails and v1/v2
//! upgrades create no copy (K3), retention keeps the newest three copies
//! (K4), a failed copy fails `open` with `log.bin` unchanged (K5), and
//! `clear` removes the copies or fails without publishing (K6).
//!
//! Tests assert on the file system directly — `fs::read`, `fs::metadata`,
//! `sibling_names` — never through the store's quarantine helpers.

use super::*;

/// Sibling names of `path` that are quarantine copies: `log.corrupt-`
/// followed by the stamp shape (8 digits, `T`, 6 digits, `Z`) and an
/// optional `.{digits}.{digits}` tail. Matches the documented copy-name
/// contract without calling the module under test.
fn corrupt_copy_names(path: &Path) -> Vec<String> {
    sibling_names(path)
        .into_iter()
        .filter(|name| {
            let Some(rest) = name.strip_prefix("log.corrupt-") else {
                return false;
            };
            if rest.len() < 16 {
                return false;
            }
            let (stamp, tail) = rest.split_at(16);
            let bytes = stamp.as_bytes();
            let digits = |slice: &[u8]| slice.iter().all(|byte| byte.is_ascii_digit());
            if !digits(&bytes[..8])
                || bytes[8] != b'T'
                || !digits(&bytes[9..15])
                || bytes[15] != b'Z'
            {
                return false;
            }
            tail.is_empty()
                || tail.strip_prefix('.').is_some_and(|tail| {
                    let mut parts = tail.split('.');
                    matches!(
                        (parts.next(), parts.next(), parts.next()),
                        (Some(pid), Some(id), None)
                            if [pid, id].iter().all(|part| {
                                !part.is_empty()
                                    && part.bytes().all(|byte| byte.is_ascii_digit())
                            })
                    )
                })
        })
        .collect()
}

/// The file offsets of every complete frame after the header: `(payload
/// start, frame end)` for each frame whose declared length is readable.
fn frame_spans(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut offset = HEADER_LEN;
    while offset + RECORD_ENVELOPE_LEN <= bytes.len() {
        let length = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let end = offset + RECORD_ENVELOPE_LEN + length;
        if end > bytes.len() {
            break;
        }
        spans.push((offset + RECORD_ENVELOPE_LEN, end));
        offset = end;
    }
    spans
}

/// Flips one payload byte of frame `index` (0-based) in the log at `path`
/// so its CRC fails — mid-file corruption — and returns the damaged bytes.
fn damage_frame_payload(path: &Path, index: usize) -> Vec<u8> {
    let mut bytes = fs::read(path).expect("read log");
    let spans = frame_spans(&bytes);
    let (payload_start, payload_end) = spans[index];
    bytes[payload_end - 1] ^= 0xFF;
    assert!(payload_start < payload_end);
    fs::write(path, &bytes).expect("write damaged log");
    bytes
}

/// K1, K2: a damaged byte in the middle of the log is copied to a synced
/// `log.corrupt-*` sibling before `log.bin` is truncated to the valid
/// prefix; learning continues from that prefix and a later open makes no
/// second copy.
#[test]
fn mid_file_corruption_is_copied_then_truncated() {
    let path = temporary_log("quarantine-mid-file");
    {
        let service = LearningService::open(&path).expect("open");
        service.learn("かな", "加奈", 3, 4);
        service.learn("きく", "菊", 5, 6);
        service.learn("さくら", "桜", 7, 8);
    }
    let damaged = damage_frame_payload(&path, 1);
    let first_frame_end = frame_spans(&damaged)[0].1;

    let reopened = LearningService::open(&path).expect("reopen damaged log");

    let copies = corrupt_copy_names(&path);
    assert_eq!(copies.len(), 1, "exactly one quarantine copy: {copies:?}");
    assert_eq!(
        fs::read(path.parent().unwrap().join(&copies[0])).expect("copy bytes"),
        damaged,
        "the copy holds exactly what open read"
    );
    assert_eq!(
        fs::metadata(&path).expect("metadata").len() as usize,
        first_frame_end,
        "log.bin was truncated to the valid prefix"
    );
    assert_eq!(
        reopened.recovered_tail_bytes() as usize,
        damaged.len() - first_frame_end
    );
    assert!(
        history_contains(&reopened, "かな", "加奈"),
        "the record before the damage survived"
    );
    drop(reopened);

    let truncated = fs::read(&path).expect("truncated log");
    let reopened = LearningService::open(&path).expect("second reopen");
    assert_eq!(
        corrupt_copy_names(&path).len(),
        1,
        "a clean open makes no new copy"
    );
    assert_eq!(
        fs::read(&path).expect("log after second reopen"),
        truncated,
        "a clean open leaves log.bin unchanged"
    );
    drop(reopened);
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

/// K3: a write cut off at the end of the file is a torn tail and is
/// truncated exactly as before — no quarantine copy.
#[test]
fn torn_tails_create_no_copy() {
    for (name, tail) in [
        ("torn-short", vec![9u8, 0, 0]),
        ("torn-past-end", {
            let mut tail = 100u32.to_le_bytes().to_vec();
            tail.extend_from_slice(&0u32.to_le_bytes());
            tail
        }),
        ("torn-zeros", vec![0u8; 64]),
    ] {
        let path = temporary_log(name);
        {
            let service = LearningService::open(&path).expect("open");
            service.learn("かな", "加奈", 3, 4);
        }
        let good_len = fs::metadata(&path).expect("metadata").len();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("append")
            .write_all(&tail)
            .expect("torn tail");

        let recovered = LearningService::open(&path).expect("recover");

        assert_eq!(
            fs::metadata(&path).expect("metadata").len(),
            good_len,
            "{name}: the torn tail was truncated"
        );
        assert!(
            corrupt_copy_names(&path).is_empty(),
            "{name}: a torn tail made a copy"
        );
        drop(recovered);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }
}

/// K4: after a quarantine only the newest three matching copies remain;
/// names that do not match the copy shape are never touched.
#[test]
fn retention_keeps_the_newest_three_copies() {
    let path = temporary_log("quarantine-retention");
    {
        let service = LearningService::open(&path).expect("open");
        service.learn("かな", "加奈", 3, 4);
        service.learn("きく", "菊", 5, 6);
    }
    let directory = path.parent().unwrap();
    for (name, contents) in [
        ("log.corrupt-20000101T000000Z", b"oldest".as_slice()),
        ("log.corrupt-20010101T000000Z", b"middle".as_slice()),
        ("log.corrupt-20020101T000000Z", b"newest".as_slice()),
        ("log.corrupt-notes.txt", b"not a copy".as_slice()),
        ("other.corrupt-20000101T000000Z", b"other stem".as_slice()),
    ] {
        fs::write(directory.join(name), contents).expect("staged sibling");
    }
    damage_frame_payload(&path, 1);

    let reopened = LearningService::open(&path).expect("reopen damaged log");
    drop(reopened);

    assert!(!directory.join("log.corrupt-20000101T000000Z").exists());
    assert_eq!(
        fs::read(directory.join("log.corrupt-20010101T000000Z")).expect("2001 copy"),
        b"middle"
    );
    assert_eq!(
        fs::read(directory.join("log.corrupt-20020101T000000Z")).expect("2002 copy"),
        b"newest"
    );
    assert_eq!(
        corrupt_copy_names(&path).len(),
        3,
        "the staged copies and the fresh one are the newest three"
    );
    assert_eq!(
        fs::read(directory.join("log.corrupt-notes.txt")).expect("notes"),
        b"not a copy",
        "a non-matching name was removed"
    );
    assert_eq!(
        fs::read(directory.join("other.corrupt-20000101T000000Z")).expect("other stem"),
        b"other stem",
        "another stem's copy was removed"
    );
    let _ = fs::remove_dir_all(directory);
}

/// Removes the directory write deny this guard installs, before the test's
/// temporary directory is deleted.
#[cfg(windows)]
struct DenyWriteGuard(PathBuf);

#[cfg(windows)]
impl DenyWriteGuard {
    /// Denies Everyone write-data on `directory` with no inheritance flags,
    /// so existing files stay writable but no new file can be created.
    fn new(directory: &Path) -> Self {
        let status = std::process::Command::new("icacls")
            .arg(directory)
            .args(["/deny", "*S-1-1-0:(WD)"])
            .status()
            .expect("icacls deny");
        assert!(status.success(), "icacls deny failed");
        Self(directory.to_owned())
    }
}

#[cfg(windows)]
impl Drop for DenyWriteGuard {
    fn drop(&mut self) {
        let status = std::process::Command::new("icacls")
            .arg(&self.0)
            .args(["/remove:d", "*S-1-1-0"])
            .status()
            .expect("icacls remove");
        assert!(status.success(), "icacls remove failed");
    }
}

/// K5: when the copy cannot be made, `open` fails and `log.bin` is left
/// byte-for-byte unchanged; the next open retries and succeeds.
#[cfg(windows)]
#[test]
fn a_failed_copy_changes_nothing_and_open_retries() {
    let path = temporary_log("quarantine-denied");
    {
        let service = LearningService::open(&path).expect("open");
        service.learn("かな", "加奈", 3, 4);
        service.learn("きく", "菊", 5, 6);
    }
    let damaged = damage_frame_payload(&path, 1);
    let directory = path.parent().unwrap().to_owned();
    let _deny = DenyWriteGuard::new(&directory);
    assert!(
        File::create(directory.join("probe")).is_err(),
        "the deny did not block new files"
    );

    assert!(
        LearningService::open(&path).is_err(),
        "open succeeded without the quarantine copy it requires"
    );
    assert_eq!(
        fs::read(&path).expect("log after failed open"),
        damaged,
        "a failed copy still truncated log.bin"
    );
    assert!(corrupt_copy_names(&path).is_empty());
    drop(_deny);

    let reopened = LearningService::open(&path).expect("retry after removing the deny");
    assert_eq!(corrupt_copy_names(&path).len(), 1, "the retry copied");
    assert!(
        fs::metadata(&path).expect("metadata").len() < damaged.len() as u64,
        "the retry truncated the log"
    );
    drop(reopened);
    let _ = fs::remove_dir_all(&directory);
}

/// K6: `clear` removes every matching copy before it publishes the empty
/// log; a name that does not match is left alone.
#[test]
fn clear_removes_quarantine_copies() {
    let path = temporary_log("quarantine-clear");
    {
        let service = LearningService::open(&path).expect("open");
        service.learn("かな", "加奈", 3, 4);
        service.learn("きく", "菊", 5, 6);
    }
    damage_frame_payload(&path, 1);
    let service = LearningService::open(&path).expect("reopen damaged log");
    assert_eq!(corrupt_copy_names(&path).len(), 1);
    let directory = path.parent().unwrap();
    fs::write(directory.join("log.corrupt-20010101T000000Z"), b"staged").expect("staged copy");
    fs::write(directory.join("log.corrupt-notes.txt"), b"not a copy").expect("staged notes");

    service.clear().expect("clear");

    assert!(
        corrupt_copy_names(&path).is_empty(),
        "clear left a quarantine copy"
    );
    assert_eq!(
        fs::read(directory.join("log.corrupt-notes.txt")).expect("notes"),
        b"not a copy",
        "clear removed a non-matching name"
    );
    drop(service);
    let _ = fs::remove_dir_all(directory);
}

/// K6 failure: when a copy cannot be removed, `clear` fails without
/// publishing, so `log.bin` and the copy both survive.
#[cfg(windows)]
#[test]
fn clear_fails_closed_when_a_copy_cannot_be_removed() {
    let path = temporary_log("quarantine-clear-held");
    {
        let service = LearningService::open(&path).expect("open");
        service.learn("かな", "加奈", 3, 4);
    }
    let directory = path.parent().unwrap();
    let copy = directory.join("log.corrupt-20200101T000000Z");
    fs::write(&copy, b"staged copy").expect("staged copy");
    let held = hold_without_delete_share(&copy);
    let service = LearningService::open(&path).expect("reopen");

    assert!(
        service.clear().is_err(),
        "clear published while a copy could not be removed"
    );
    assert!(copy.exists(), "the held copy was removed anyway");
    drop(held);
    drop(service);

    let reopened = LearningService::open(&path).expect("reopen after failed clear");
    assert!(
        history_contains(&reopened, "かな", "加奈"),
        "a failed clear still emptied the log"
    );
    drop(reopened);
    let _ = fs::remove_dir_all(directory);
}

/// K3: a v1 log's damaged frame needs no copy — `publish_upgrade` already
/// keeps the original bytes in `log.v1.bak`.
#[test]
fn a_v1_upgrade_creates_no_copy() {
    let path = temporary_log("quarantine-v1");
    // Two v1 frames: day, reading_len, surface_len, reading, surface.
    let frame = |reading: &str, surface: &str| {
        let mut payload = Vec::new();
        payload.extend_from_slice(&unix_day().to_le_bytes());
        payload.extend_from_slice(&(reading.len() as u16).to_le_bytes());
        payload.extend_from_slice(&(surface.len() as u16).to_le_bytes());
        payload.extend_from_slice(reading.as_bytes());
        payload.extend_from_slice(surface.as_bytes());
        let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
        frame.extend_from_slice(&crc32(&payload).to_le_bytes());
        frame.extend_from_slice(&payload);
        frame
    };
    let mut old = header(FORMAT_VERSION_1).to_vec();
    old.extend_from_slice(&frame("かな", "加奈"));
    let second_payload_byte = old.len() + RECORD_ENVELOPE_LEN;
    old.extend_from_slice(&frame("きく", "菊"));
    old[second_payload_byte] ^= 0xFF;
    fs::write(&path, &old).expect("write damaged v1 log");

    let upgraded = LearningService::open(&path).expect("upgrade damaged v1 log");

    assert!(
        corrupt_copy_names(&path).is_empty(),
        "a v1 upgrade made a quarantine copy"
    );
    assert_eq!(
        fs::read(path.with_extension("v1.bak")).expect("v1 backup"),
        old,
        "the upgrade backup does not hold the damaged original"
    );
    assert!(history_contains(&upgraded, "かな", "加奈"));
    drop(upgraded);
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}
