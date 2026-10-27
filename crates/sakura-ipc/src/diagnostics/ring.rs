//! Ring retention for a bounded diagnostics log (#313).
//!
//! The append-only layout stopped at its byte ceiling and then refused every
//! later event, so a log that filled in three weeks answered nothing about the
//! week after. This layout keeps the newest records instead: the file is
//! exactly the ceiling long, its first 32 bytes are a header, and the rest is a
//! ring of fixed record slots that the next event overwrites oldest first.
//!
//! Writers in different host processes serialise on an exclusive lock over the
//! whole file and never wait for it: a writer that finds it held drops its
//! event and says so, and the next event the same process stores adds the loss
//! to the header's drop counter. Nothing retries, so a stuck or slow peer can
//! cost diagnostics but never input latency. Windows byte-range locks are
//! mandatory, so a build from before this layout cannot write through the lock
//! either; outside it, it sees a full-length file and refuses as it always did.
//!
//! Ordering contract: slot `1 + n % slots` holds the `n`th stored event, so the
//! ring is in claim order and an event only ever replaces the oldest one. The
//! one exception is a converted append-only log: its first record moves to the
//! slot after its last one, so of those records it is overwritten last instead
//! of first. The aggregates read from the ring do not depend on order; first
//! and last timestamps are the minimum and maximum over the retained records.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read};
use std::os::windows::fs::FileExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::{crc32, RECORD_BYTES};

const RING_MAGIC: &[u8; 4] = b"SKRG";
const RING_VERSION: u16 = 1;
const SLOT_BYTES: u64 = RECORD_BYTES as u64;

/// A reader or `clear` is not on the key path, so unlike a writer it may wait
/// for a writer's lock, but only this long: a writer holds it for a few small
/// writes, and a peer that never lets go must not hang the settings tool.
const LOCK_ATTEMPTS: u32 = 50;
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(2);

/// How a diagnostics log is retaining its records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    /// No file, or an empty one: nothing has been stored since the last clear.
    Empty,
    /// The append-only file a build before ring retention wrote. While it is
    /// at capacity such a build refuses every new event. The next event this
    /// build stores converts the file in place, keeping its records.
    Legacy { reached_capacity: bool },
    /// The ring. `recorded` counts every event stored since the last clear,
    /// so `recorded - slots` of them were overwritten to keep the newest.
    /// `dropped` counts events that never reached a slot: another writer held
    /// the lock, storage failed, or the conversion had no room for one.
    Ring {
        slots: u64,
        recorded: u64,
        dropped: u64,
    },
    /// The file starts with a ring header that fails validation. Its slots
    /// are still read, but a writer cannot tell which slot is next, so every
    /// new event is dropped until `diagnostics clear` resets the file.
    InvalidRingHeader,
}

impl Retention {
    /// Events overwritten, oldest first, to keep the newest ones.
    pub fn overwritten(&self) -> u64 {
        match self {
            Self::Ring {
                slots, recorded, ..
            } => recorded.saturating_sub(*slots),
            _ => 0,
        }
    }
}

/// One consistent read of a log: how it retains records, and those records.
pub(super) struct Snapshot {
    bytes: Vec<u8>,
    /// Byte range of `bytes` holding record slots.
    records: std::ops::Range<usize>,
    pub(super) retention: Retention,
    /// Bytes that are not a whole record inside the ceiling, or lie beyond it.
    pub(super) ignored_tail_bytes: u64,
}

impl Snapshot {
    /// Every slot that holds something. A slot of zeros was never written, so
    /// it is neither a record nor a corrupt one.
    pub(super) fn records(&self) -> impl Iterator<Item = &[u8]> {
        self.bytes[self.records.clone()]
            .chunks_exact(RECORD_BYTES)
            .filter(|slot| slot.iter().any(|byte| *byte != 0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Header {
    recorded: u64,
    dropped: u32,
}

enum HeaderState {
    /// Not a ring: the first bytes are a record or nothing at all.
    Absent,
    Valid(Header),
    Invalid,
}

fn slot_count(ceiling: u64) -> u64 {
    ceiling / SLOT_BYTES - 1
}

fn encode_header(magic: &[u8; 4], slots: u64, header: Header) -> [u8; RECORD_BYTES] {
    let mut bytes = [0u8; RECORD_BYTES];
    bytes[..4].copy_from_slice(RING_MAGIC);
    bytes[4..6].copy_from_slice(&RING_VERSION.to_le_bytes());
    // The header names the record magic it holds, so the other log's reader
    // rejects the file as a whole instead of reading its slots.
    bytes[8..12].copy_from_slice(magic);
    bytes[12..16].copy_from_slice(&u32::try_from(slots).unwrap_or(u32::MAX).to_le_bytes());
    bytes[16..24].copy_from_slice(&header.recorded.to_le_bytes());
    bytes[24..28].copy_from_slice(&header.dropped.to_le_bytes());
    let checksum = crc32(&bytes[..28]);
    bytes[28..].copy_from_slice(&checksum.to_le_bytes());
    bytes
}

fn decode_header(bytes: &[u8], magic: &[u8; 4], slots: u64) -> HeaderState {
    if bytes.len() < RECORD_BYTES || &bytes[..4] != RING_MAGIC {
        return HeaderState::Absent;
    }
    let field = |range: std::ops::Range<usize>| -> [u8; 4] {
        bytes[range].try_into().expect("four-byte header field")
    };
    let valid = u16::from_le_bytes([bytes[4], bytes[5]]) == RING_VERSION
        && bytes[8..12] == magic[..]
        && u64::from(u32::from_le_bytes(field(12..16))) == slots
        && crc32(&bytes[..28]) == u32::from_le_bytes(field(28..32));
    if !valid {
        return HeaderState::Invalid;
    }
    HeaderState::Valid(Header {
        recorded: u64::from_le_bytes(bytes[16..24].try_into().expect("eight-byte field")),
        dropped: u32::from_le_bytes(field(24..28)),
    })
}

/// Stores one encoded record, keeping the newest `ceiling / 32 - 1` of them.
///
/// A failure of any kind leaves the file as valid as it was, is returned to
/// the caller, and is counted in `pending_drops` so the next event this
/// process stores can report it. Nothing here waits or retries.
pub(super) fn append(
    path: &Path,
    magic: &[u8; 4],
    ceiling: u64,
    record: &[u8; RECORD_BYTES],
    kind: &str,
    pending_drops: &AtomicU64,
) -> io::Result<()> {
    let result = append_locked(path, magic, ceiling, record, kind, pending_drops);
    if result.is_err() {
        pending_drops.fetch_add(1, Ordering::Relaxed);
    }
    result
}

fn append_locked(
    path: &Path,
    magic: &[u8; 4],
    ceiling: u64,
    record: &[u8; RECORD_BYTES],
    kind: &str,
    pending_drops: &AtomicU64,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("another writer holds the IPC {kind} diagnostics"),
            ));
        }
        Err(TryLockError::Error(error)) => return Err(error),
    }

    let slots = slot_count(ceiling);
    let mut header = load_or_convert(&file, magic, ceiling, slots, kind)?;
    let slot = 1 + header.recorded % slots;
    // The record goes in before the header claims it. If the header write
    // then fails, the slot is reused by the next event rather than left
    // claimed with an older lap's record still in it.
    write_all_at(&file, record, slot * SLOT_BYTES)?;
    header.recorded = header.recorded.saturating_add(1);
    let pending = pending_drops.swap(0, Ordering::Relaxed);
    header.dropped = header
        .dropped
        .saturating_add(u32::try_from(pending).unwrap_or(u32::MAX));
    if let Err(error) = write_all_at(&file, &encode_header(magic, slots, header), 0) {
        pending_drops.fetch_add(pending, Ordering::Relaxed);
        return Err(error);
    }
    Ok(())
}

/// Returns the ring's header, first making the locked file a ring if it is
/// not one yet. Every conversion step leaves a file a later writer can still
/// convert or read.
fn load_or_convert(
    file: &File,
    magic: &[u8; 4],
    ceiling: u64,
    slots: u64,
    kind: &str,
) -> io::Result<Header> {
    let len = file.metadata()?.len();
    let mut first = [0u8; RECORD_BYTES];
    if len >= SLOT_BYTES {
        read_exact_at(file, &mut first, 0)?;
    }
    match decode_header(&first, magic, slots) {
        HeaderState::Valid(mut header) => {
            if len != ceiling {
                // Longer only when a build before the ring checked the size
                // before the conversion and appended after it. That record
                // lies outside every slot, so it is reported as dropped.
                header.dropped = header
                    .dropped
                    .saturating_add(overflow_records(len, ceiling));
                file.set_len(ceiling)?;
            }
            Ok(header)
        }
        HeaderState::Invalid => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "IPC {kind} diagnostics have an invalid ring header; `diagnostics clear` resets it"
            ),
        )),
        HeaderState::Absent => convert_legacy(file, magic, ceiling, slots, len),
    }
}

/// Converts an append-only log in place. Its records already sit on slot
/// boundaries, so only the first one has to move to make room for the header.
fn convert_legacy(
    file: &File,
    magic: &[u8; 4],
    ceiling: u64,
    slots: u64,
    len: u64,
) -> io::Result<Header> {
    let kept = len.min(ceiling);
    let mut bytes = vec![0u8; usize::try_from(kept).unwrap_or(usize::MAX)];
    read_exact_at(file, &mut bytes, 0)?;
    // Count up to the last slot holding anything, not the file length: a
    // conversion interrupted after extending the file must not make the next
    // attempt read the zero-filled extension as a full log.
    let mut occupied = bytes
        .chunks_exact(RECORD_BYTES)
        .rposition(|slot| slot.iter().any(|byte| *byte != 0))
        .map_or(0, |last| last as u64 + 1);
    // A conversion interrupted between moving the first record and writing
    // the header leaves that record twice. A record carries its millisecond
    // timestamp, process and thread, so an exact copy can only be that move.
    if occupied > 1 {
        let last = (occupied - 1) as usize * RECORD_BYTES;
        if bytes[last..last + RECORD_BYTES] == bytes[..RECORD_BYTES] {
            occupied -= 1;
        }
    }
    let torn = u32::from(!kept.is_multiple_of(SLOT_BYTES));
    let mut header = Header {
        recorded: 0,
        dropped: torn.saturating_add(overflow_records(len, ceiling)),
    };
    if occupied > slots {
        // A full legacy log: its oldest record gives way to the header,
        // exactly as the ring would have overwritten it.
        header.recorded = slots;
        header.dropped = header.dropped.saturating_add(1);
    } else if occupied > 0 {
        write_all_at(file, &bytes[..RECORD_BYTES], occupied * SLOT_BYTES)?;
        header.recorded = occupied;
    }
    write_all_at(file, &encode_header(magic, slots, header), 0)?;
    file.set_len(ceiling)?;
    Ok(header)
}

fn overflow_records(len: u64, ceiling: u64) -> u32 {
    u32::try_from(len.saturating_sub(ceiling).div_ceil(SLOT_BYTES)).unwrap_or(u32::MAX)
}

/// Reads the log under a shared lock, so a writer's record and header are
/// seen together or not at all.
pub(super) fn read(path: &Path, magic: &[u8; 4], ceiling: u64, kind: &str) -> io::Result<Snapshot> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Snapshot {
                bytes: Vec::new(),
                records: 0..0,
                retention: Retention::Empty,
                ignored_tail_bytes: 0,
            });
        }
        Err(error) => return Err(error),
    };
    lock_bounded(kind, || file.try_lock_shared())?;
    let mut bytes = Vec::new();
    (&file).read_to_end(&mut bytes)?;
    Ok(classify(bytes, magic, ceiling))
}

fn classify(bytes: Vec<u8>, magic: &[u8; 4], ceiling: u64) -> Snapshot {
    let len = bytes.len();
    let within = len.min(usize::try_from(ceiling).unwrap_or(usize::MAX));
    let slots = slot_count(ceiling);
    let (retention, start) = match decode_header(&bytes, magic, slots) {
        _ if len == 0 => (Retention::Empty, 0),
        HeaderState::Absent => (
            Retention::Legacy {
                reached_capacity: len as u64 >= ceiling,
            },
            0,
        ),
        HeaderState::Valid(header) => (
            Retention::Ring {
                slots,
                recorded: header.recorded,
                dropped: u64::from(header.dropped),
            },
            RECORD_BYTES,
        ),
        HeaderState::Invalid => (Retention::InvalidRingHeader, RECORD_BYTES),
    };
    let start = start.min(within);
    let end = start + (within - start) / RECORD_BYTES * RECORD_BYTES;
    Snapshot {
        ignored_tail_bytes: (len - end) as u64,
        bytes,
        records: start..end,
        retention,
    }
}

/// Empties the log. The next event starts a fresh ring.
pub(super) fn clear(path: &Path, kind: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    lock_bounded(kind, || file.try_lock())?;
    file.set_len(0)?;
    file.sync_all()
}

fn lock_bounded(
    kind: &str,
    mut try_lock: impl FnMut() -> Result<(), TryLockError>,
) -> io::Result<()> {
    for attempt in 1..=LOCK_ATTEMPTS {
        match try_lock() {
            Ok(()) => return Ok(()),
            Err(TryLockError::WouldBlock) if attempt < LOCK_ATTEMPTS => {
                std::thread::sleep(LOCK_RETRY_DELAY);
            }
            Err(TryLockError::WouldBlock) => break,
            Err(TryLockError::Error(error)) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        format!("IPC {kind} diagnostics stayed locked by another process"),
    ))
}

fn write_all_at(file: &File, mut bytes: &[u8], mut offset: u64) -> io::Result<()> {
    // Positional and under the exclusive lock, so finishing a short write at
    // the right offset cannot interleave with another writer's bytes.
    while !bytes.is_empty() {
        match file.seek_write(bytes, offset)? {
            0 => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            written => {
                bytes = &bytes[written..];
                offset += written as u64;
            }
        }
    }
    Ok(())
}

fn read_exact_at(file: &File, mut bytes: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !bytes.is_empty() {
        match file.seek_read(bytes, offset)? {
            0 => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            read => {
                bytes = &mut bytes[read..];
                offset += read as u64;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::encode_record;
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Instant;

    const MAGIC: &[u8; 4] = b"SKZZ";
    /// Eight record slots after the header: small enough to wrap many times.
    const CEILING: u64 = 9 * SLOT_BYTES;

    static NEXT_FILE: AtomicU64 = AtomicU64::new(1);

    fn temporary_file(name: &str) -> PathBuf {
        let id = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join("sakura-input-tests")
            .join(format!(
                "sakura-ipc-ring-{}-{name}-{id}.bin",
                std::process::id()
            ))
    }

    fn record(detail: u32) -> [u8; RECORD_BYTES] {
        encode_record(MAGIC, 1, detail)
    }

    fn detail_of(record: &[u8]) -> u32 {
        u32::from_le_bytes(record[24..28].try_into().expect("detail field"))
    }

    fn details(snapshot: &Snapshot) -> Vec<u32> {
        let mut details: Vec<u32> = snapshot.records().map(detail_of).collect();
        details.sort_unstable();
        details
    }

    fn store(path: &Path, detail: u32, pending: &AtomicU64) -> io::Result<()> {
        append(path, MAGIC, CEILING, &record(detail), "test", pending)
    }

    fn ring(slots: u64, recorded: u64, dropped: u64) -> Retention {
        Retention::Ring {
            slots,
            recorded,
            dropped,
        }
    }

    fn legacy_file(path: &Path, details: impl IntoIterator<Item = u32>) -> Vec<u8> {
        let bytes: Vec<u8> = details.into_iter().flat_map(record).collect();
        fs::create_dir_all(path.parent().expect("parent")).expect("test directory");
        fs::write(path, &bytes).expect("legacy fixture");
        bytes
    }

    #[test]
    fn wrapping_keeps_the_newest_records_in_claim_order_within_the_ceiling() {
        let path = temporary_file("wrap");
        let pending = AtomicU64::new(0);
        for detail in 1..=20 {
            store(&path, detail, &pending).expect("store");
            assert_eq!(fs::metadata(&path).expect("metadata").len(), CEILING);
        }
        let snapshot = read(&path, MAGIC, CEILING, "test").expect("read");
        let bytes = fs::read(&path).expect("bytes");
        let _ = fs::remove_file(&path);

        assert_eq!(snapshot.retention, ring(8, 20, 0));
        assert_eq!(snapshot.retention.overwritten(), 12);
        assert_eq!(snapshot.ignored_tail_bytes, 0);
        assert_eq!(details(&snapshot), (13..=20).collect::<Vec<_>>());
        // The ordering contract: the nth stored event (from zero) is in slot
        // 1 + n % slots, so each new event replaced exactly the oldest one.
        for detail in 13..=20u32 {
            let slot = 1 + u64::from(detail - 1) % 8;
            let offset = usize::try_from(slot * SLOT_BYTES).expect("offset");
            assert_eq!(detail_of(&bytes[offset..offset + RECORD_BYTES]), detail);
        }
    }

    #[test]
    fn a_held_lock_drops_the_event_at_once_and_the_next_store_reports_it() {
        let path = temporary_file("contended");
        let pending = AtomicU64::new(0);
        store(&path, 1, &pending).expect("first");
        let before = fs::read(&path).expect("before");

        let holder = File::open(&path).expect("holder");
        holder
            .try_lock()
            .expect("hold the lock as another process would");
        let started = Instant::now();
        let error = store(&path, 2, &pending).expect_err("lock is held");
        let waited = started.elapsed();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(waited < Duration::from_millis(500), "waited {waited:?}");
        assert_eq!(pending.load(Ordering::Relaxed), 1);
        drop(holder);
        assert_eq!(fs::read(&path).expect("after"), before);

        store(&path, 3, &pending).expect("lock released");
        let snapshot = read(&path, MAGIC, CEILING, "test").expect("read");
        let _ = fs::remove_file(&path);
        assert_eq!(pending.load(Ordering::Relaxed), 0);
        assert_eq!(snapshot.retention, ring(8, 2, 1));
        assert_eq!(details(&snapshot), vec![1, 3]);
    }

    #[test]
    fn a_storage_failure_is_returned_and_counted_but_never_retried() {
        let blocker = temporary_file("not-a-directory");
        fs::create_dir_all(blocker.parent().expect("parent")).expect("test directory");
        fs::write(&blocker, b"a file where a directory should be").expect("blocker");
        let unreachable = blocker.join("ipc-disconnects.bin");
        let pending = AtomicU64::new(0);

        let started = Instant::now();
        assert!(store(&unreachable, 1, &pending).is_err());
        assert!(store(&unreachable, 2, &pending).is_err());
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(pending.load(Ordering::Relaxed), 2);

        // The loss becomes visible with the next event that is stored.
        let path = temporary_file("after-failure");
        store(&path, 3, &pending).expect("store");
        let snapshot = read(&path, MAGIC, CEILING, "test").expect("read");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&blocker);
        assert_eq!(snapshot.retention, ring(8, 1, 2));
    }

    #[test]
    fn concurrent_writers_never_overwrite_each_other_or_grow_the_file() {
        for (writers, events, ceiling) in [(4u32, 16u32, 65 * SLOT_BYTES), (4, 50, CEILING)] {
            let path = Arc::new(temporary_file("concurrent"));
            let pending = Arc::new(AtomicU64::new(0));
            let handles: Vec<_> = (0..writers)
                .map(|writer| {
                    let path = Arc::clone(&path);
                    let pending = Arc::clone(&pending);
                    std::thread::spawn(move || {
                        for event in 0..events {
                            let detail = writer * 1_000 + event + 1;
                            let _ =
                                append(&path, MAGIC, ceiling, &record(detail), "test", &pending);
                            let len = fs::metadata(&*path).expect("metadata").len();
                            assert!(len <= ceiling, "grew to {len}");
                        }
                    })
                })
                .collect();
            for handle in handles {
                handle.join().expect("writer thread");
            }
            let snapshot = read(&path, MAGIC, ceiling, "test").expect("read");
            let len = fs::metadata(&*path).expect("metadata").len();
            let _ = fs::remove_file(&*path);

            assert_eq!(len, ceiling);
            let Retention::Ring {
                slots,
                recorded,
                dropped,
            } = snapshot.retention
            else {
                panic!("not a ring: {:?}", snapshot.retention);
            };
            // Every attempt is accounted for exactly once: stored, reported
            // as dropped, or still pending in this process.
            assert_eq!(
                recorded + dropped + pending.load(Ordering::Relaxed),
                u64::from(writers * events)
            );
            let retained = details(&snapshot);
            let mut unique = retained.clone();
            unique.dedup();
            assert_eq!(unique, retained, "a record was duplicated");
            assert_eq!(retained.len() as u64, recorded.min(slots));
            for slot in snapshot.records() {
                assert_eq!(&slot[..4], MAGIC, "torn or foreign slot");
                let checksum = u32::from_le_bytes(slot[28..32].try_into().expect("crc"));
                assert_eq!(crc32(&slot[..28]), checksum, "torn slot");
            }
        }
    }

    #[test]
    fn a_legacy_log_is_converted_in_place_keeping_every_record() {
        let path = temporary_file("legacy");
        legacy_file(&path, 1..=5);
        let before = read(&path, MAGIC, CEILING, "test").expect("legacy read");
        assert_eq!(
            before.retention,
            Retention::Legacy {
                reached_capacity: false
            }
        );
        assert_eq!(details(&before), vec![1, 2, 3, 4, 5]);

        let pending = AtomicU64::new(0);
        store(&path, 6, &pending).expect("convert and store");
        let after = read(&path, MAGIC, CEILING, "test").expect("ring read");
        let len = fs::metadata(&path).expect("metadata").len();
        let _ = fs::remove_file(&path);
        assert_eq!(len, CEILING);
        assert_eq!(after.retention, ring(8, 6, 0));
        assert_eq!(details(&after), vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn a_full_legacy_log_gives_only_its_oldest_record_to_the_header() {
        let path = temporary_file("legacy-full");
        legacy_file(&path, 1..=9);
        let before = read(&path, MAGIC, CEILING, "test").expect("legacy read");
        assert_eq!(
            before.retention,
            Retention::Legacy {
                reached_capacity: true
            }
        );

        let pending = AtomicU64::new(0);
        store(&path, 10, &pending).expect("convert and store");
        let after = read(&path, MAGIC, CEILING, "test").expect("ring read");
        let _ = fs::remove_file(&path);
        // Record 1 made room for the header and record 2 for event 10: the
        // two oldest, as a ring that had held them all along would have done.
        assert_eq!(after.retention, ring(8, 9, 1));
        assert_eq!(details(&after), (3..=10).collect::<Vec<_>>());
    }

    #[test]
    fn a_conversion_interrupted_after_moving_the_first_record_counts_it_once() {
        let path = temporary_file("legacy-interrupted-move");
        let mut bytes = legacy_file(&path, 1..=3);
        let first = bytes[..RECORD_BYTES].to_vec();
        bytes.extend_from_slice(&first);
        fs::write(&path, &bytes).expect("moved but no header yet");
        let pending = AtomicU64::new(0);
        store(&path, 4, &pending).expect("convert");
        let snapshot = read(&path, MAGIC, CEILING, "test").expect("read");
        let _ = fs::remove_file(&path);
        assert_eq!(snapshot.retention, ring(8, 4, 0));
        assert_eq!(details(&snapshot), vec![1, 2, 3, 4]);
    }

    #[test]
    fn a_conversion_interrupted_before_extending_the_file_completes_later() {
        let path = temporary_file("interrupted");
        let pending = AtomicU64::new(0);
        store(&path, 1, &pending).expect("store");
        store(&path, 2, &pending).expect("store");
        let bytes = fs::read(&path).expect("bytes");
        // Header and two slots: what a conversion leaves if it stops after
        // writing the header and before `set_len`.
        fs::write(&path, &bytes[..3 * RECORD_BYTES]).expect("short ring");
        store(&path, 3, &pending).expect("store");
        let snapshot = read(&path, MAGIC, CEILING, "test").expect("read");
        let len = fs::metadata(&path).expect("metadata").len();
        let _ = fs::remove_file(&path);
        assert_eq!(len, CEILING);
        assert_eq!(details(&snapshot), vec![1, 2, 3]);
        assert_eq!(snapshot.retention, ring(8, 3, 0));
    }

    #[test]
    fn torn_and_overflowing_bytes_are_reported_and_never_counted_as_records() {
        // A torn final append in a legacy log is reported as dropped when the
        // log is converted, and the slot it occupied is reused.
        let path = temporary_file("legacy-torn");
        let mut bytes = legacy_file(&path, 1..=3);
        bytes.extend_from_slice(&[0xAB; 5]);
        fs::write(&path, &bytes).expect("torn fixture");
        let torn = read(&path, MAGIC, CEILING, "test").expect("torn read");
        assert_eq!(torn.ignored_tail_bytes, 5);
        let pending = AtomicU64::new(0);
        store(&path, 4, &pending).expect("convert");
        let converted = read(&path, MAGIC, CEILING, "test").expect("ring read");
        assert_eq!(converted.retention, ring(8, 4, 1));
        assert_eq!(details(&converted), vec![1, 2, 3, 4]);

        // A build from before the ring can win a race against the
        // conversion and append past the ceiling. The reader ignores those
        // bytes; the next store trims them and reports the record as dropped.
        let mut overflowed = fs::read(&path).expect("ring bytes");
        overflowed.extend_from_slice(&record(99));
        fs::write(&path, &overflowed).expect("overflow fixture");
        let seen = read(&path, MAGIC, CEILING, "test").expect("overflow read");
        assert_eq!(seen.ignored_tail_bytes, SLOT_BYTES);
        assert_eq!(details(&seen), vec![1, 2, 3, 4]);
        store(&path, 5, &pending).expect("store");
        let trimmed = read(&path, MAGIC, CEILING, "test").expect("trimmed read");
        let len = fs::metadata(&path).expect("metadata").len();
        let _ = fs::remove_file(&path);
        assert_eq!(len, CEILING);
        assert_eq!(trimmed.retention, ring(8, 5, 2));
    }

    #[test]
    fn an_invalid_header_refuses_new_events_but_keeps_its_slots_readable() {
        let path = temporary_file("bad-header");
        let pending = AtomicU64::new(0);
        store(&path, 1, &pending).expect("store");
        store(&path, 2, &pending).expect("store");
        let mut bytes = fs::read(&path).expect("bytes");
        bytes[20] ^= 0xFF;
        fs::write(&path, &bytes).expect("corrupt header");

        let error = store(&path, 3, &pending).expect_err("header is invalid");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(pending.load(Ordering::Relaxed), 1);
        assert_eq!(fs::read(&path).expect("unchanged"), bytes);
        let snapshot = read(&path, MAGIC, CEILING, "test").expect("read");
        assert_eq!(snapshot.retention, Retention::InvalidRingHeader);
        assert_eq!(details(&snapshot), vec![1, 2]);

        clear(&path, "test").expect("clear");
        let cleared = read(&path, MAGIC, CEILING, "test").expect("cleared");
        assert_eq!(cleared.retention, Retention::Empty);
        store(&path, 4, &pending).expect("fresh ring");
        let fresh = read(&path, MAGIC, CEILING, "test").expect("read");
        let _ = fs::remove_file(&path);
        assert_eq!(fresh.retention, ring(8, 1, 1));
    }

    #[test]
    fn a_ring_of_the_other_log_is_never_read_as_this_one() {
        let path = temporary_file("other-magic");
        let pending = AtomicU64::new(0);
        let other = encode_record(b"SKYY", 1, 7);
        append(&path, b"SKYY", CEILING, &other, "other", &pending).expect("other log");
        let snapshot = read(&path, MAGIC, CEILING, "test").expect("read");
        let error = store(&path, 1, &pending).expect_err("the other log's header");
        let _ = fs::remove_file(&path);
        assert_eq!(snapshot.retention, Retention::InvalidRingHeader);
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_reader_waits_for_a_writer_only_briefly() {
        let path = temporary_file("reader-bounded");
        let pending = AtomicU64::new(0);
        store(&path, 1, &pending).expect("store");
        let holder = File::open(&path).expect("holder");
        holder.try_lock().expect("hold the lock");
        let started = Instant::now();
        let error = read(&path, MAGIC, CEILING, "test")
            .err()
            .expect("lock is held");
        let waited = started.elapsed();
        drop(holder);
        let _ = fs::remove_file(&path);
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(waited < Duration::from_secs(2), "waited {waited:?}");
    }
}
