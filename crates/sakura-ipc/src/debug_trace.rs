//! Developer-mode branch trace for Dual TSF, candidate UI, and conversion.
//!
//! Records are fixed tokens plus integers. There is no API that accepts user
//! text, process names, or candidate surfaces. Emission is a no-op until
//! explicitly enabled.
//!
//! # Retention
//!
//! The trace keeps its newest rows. It is two files next to each other:
//! `debug.tsv` is the current generation and `debug.1.tsv` the previous one,
//! each limited to about [`MAX_GENERATION_BYTES`]. The writer that finds the
//! current generation full renames it over the previous one, and the next row
//! starts a fresh file. After a long session the trace therefore holds between
//! one and two generations of the most recent rows, where a full append-only
//! file used to keep the first two MiB and refuse everything after them.
//!
//! The engine and the TSF DLL in every host process all write, so rotation is
//! guarded by a non-blocking exclusive lock on `debug.lock`, an empty file that
//! exists only to be locked. The holder re-checks the size *by path* under the
//! lock; without that, a writer that saw the file full a moment earlier could
//! rename the generation another process had just started over the one it was
//! meant to keep. A writer that cannot take the lock drops its row. Nothing
//! here waits or loops: after any failed attempt (lock held, rename refused,
//! directory unwritable) [`emit`] stays silent for a back-off period that
//! doubles up to a minute, so a trace that cannot be written costs the key path
//! one atomic load instead of several system calls per key.
//!
//! A tool that holds `debug.tsv` or `debug.1.tsv` open without
//! `FILE_SHARE_DELETE` makes the rename fail. The trace then behaves as a full
//! file would, dropping rows cheaply, until the tool lets go.
//!
//! Two limits are accepted rather than closed. Two processes that create a
//! fresh generation at the same instant can each write the header, so a
//! generation may carry a second header line; it ages out with that generation.
//! A trace already over the limit when this code first meets it (the old
//! 2 MiB file) is rotated whole, so the pair holds more than two generations
//! until the next rotation.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use windows::Win32::System::Threading::GetCurrentThreadId;

/// Size at which a generation is rotated away. The whole trace, the current and
/// the previous generation, therefore holds about twice this. Rows other
/// processes had already started writing can land a few hundred bytes beyond it.
pub const MAX_GENERATION_BYTES: u64 = 1024 * 1024;

const HEADER: &str = "unix_ms\tpid\ttid\tcomp\tinst\tevent\tdecision\tk0\tk1\tk2\tk3\n";

/// Silence after the first failed attempt. It doubles per consecutive failure.
const BACKOFF_FIRST_MS: u64 = 100;
const BACKOFF_MAX_MS: u64 = 60_000;

static ENABLED: AtomicBool = AtomicBool::new(false);
static BACKOFF: Backoff = Backoff::new();

#[derive(Debug, Clone, Copy)]
pub struct TraceEvent {
    pub component: &'static str,
    pub instance: u64,
    pub event: &'static str,
    pub decision: &'static str,
    pub k0: u64,
    pub k1: u64,
    pub k2: u64,
    pub k3: u64,
}

pub fn default_path() -> io::Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "LOCALAPPDATA is unavailable for debug trace",
        )
    })?;
    Ok(PathBuf::from(local)
        .join("SakuraInput")
        .join("logs")
        .join("debug.tsv"))
}

/// The previous generation of the trace whose current generation is `path`.
pub fn previous_path(path: &Path) -> PathBuf {
    path.with_extension("1.tsv")
}

fn lock_path(path: &Path) -> PathBuf {
    path.with_extension("lock")
}

pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Release);
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Opt-in from `SAKURA_DEBUG_TRACE=1` / `on`. Does not disable an already
/// enabled developer-mode trace.
pub fn enable_from_environment() {
    let enabled = std::env::var_os("SAKURA_DEBUG_TRACE")
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("on"));
    if enabled {
        set_enabled(true);
    }
}

/// Best-effort append. Callers on the key path ignore the result.
pub fn emit(record: TraceEvent) {
    if !is_enabled() {
        return;
    }
    let Ok(path) = default_path() else {
        return;
    };
    let _ = emit_through(
        &BACKOFF,
        monotonic_ms(),
        jitter_seed(),
        &path,
        record,
        MAX_GENERATION_BYTES,
    );
}

/// Appends one row to the trace at `path`, rotating it first when it is full.
///
/// Unlike [`emit`] this keeps no back-off state: every call tries.
pub fn emit_at(path: &Path, record: TraceEvent) -> io::Result<()> {
    write_record(path, record, MAX_GENERATION_BYTES)
}

/// [`write_record`] behind the process-wide quiet period. While `backoff` is
/// quiet this touches neither the disk nor the lock.
fn emit_through(
    backoff: &Backoff,
    now_ms: u64,
    seed: u64,
    path: &Path,
    record: TraceEvent,
    limit: u64,
) -> io::Result<()> {
    if backoff.is_quiet(now_ms) {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "debug trace is backing off after a failure",
        ));
    }
    match write_record(path, record, limit) {
        Ok(()) => {
            backoff.record_success();
            Ok(())
        }
        Err(error) => {
            backoff.record_failure(now_ms, seed);
            Err(error)
        }
    }
}

fn write_record(path: &Path, record: TraceEvent, limit: u64) -> io::Result<()> {
    append_row(path, &format_row(record), limit)
}

fn format_row(record: TraceEvent) -> String {
    debug_assert!(record.component.bytes().all(|b| b.is_ascii_graphic()));
    debug_assert!(record.event.bytes().all(|b| b.is_ascii_graphic()));
    debug_assert!(record
        .decision
        .bytes()
        .all(|b| b.is_ascii_graphic() || b == b'_'));
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        });
    // SAFETY: GetCurrentThreadId has no preconditions.
    let tid = unsafe { GetCurrentThreadId() };
    format!(
        "{timestamp}\t{}\t{tid}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
        std::process::id(),
        record.component,
        record.instance,
        record.event,
        record.decision,
        record.k0,
        record.k1,
        record.k2,
        record.k3,
    )
}

enum Append {
    Written,
    Full,
}

/// Appends `line` to the current generation. A full generation is rotated once
/// and the row retried once; whatever that leaves is the final answer.
fn append_row(path: &Path, line: &str, limit: u64) -> io::Result<()> {
    if let Append::Written = append_if_room(path, line, limit)? {
        return Ok(());
    }
    rotate(path, line.len(), limit)?;
    match append_if_room(path, line, limit)? {
        Append::Written => Ok(()),
        Append::Full => Err(io::Error::new(
            io::ErrorKind::StorageFull,
            "debug trace row does not fit a fresh generation",
        )),
    }
}

fn append_if_room(path: &Path, line: &str, limit: u64) -> io::Result<Append> {
    let mut file = open_append(path)?;
    let current = file.metadata()?.len();
    if !fits(current, line.len(), limit) {
        return Ok(Append::Full);
    }
    if current == 0 {
        // The header and the first row go out in one write so that another
        // process cannot land a row between them.
        let mut first = String::with_capacity(HEADER.len() + line.len());
        first.push_str(HEADER);
        first.push_str(line);
        write_once(&mut file, first.as_bytes())?;
    } else {
        write_once(&mut file, line.as_bytes())?;
    }
    Ok(Append::Written)
}

/// Opens the current generation for append. The folder is only created when
/// the open says it is missing, so the usual row costs no extra system calls.
fn open_append(path: &Path) -> io::Result<File> {
    let open = || OpenOptions::new().create(true).append(true).open(path);
    match open() {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            open()
        }
        opened => opened,
    }
}

fn write_once(file: &mut File, bytes: &[u8]) -> io::Result<()> {
    match file.write(bytes)? {
        written if written == bytes.len() => Ok(()),
        written => Err(io::Error::new(
            io::ErrorKind::WriteZero,
            format!("debug trace wrote {written}/{} bytes", bytes.len()),
        )),
    }
}

/// Whether a row of `row_len` bytes may still go into a generation of
/// `current` bytes. An empty generation also needs room for its header.
fn fits(current: u64, row_len: usize, limit: u64) -> bool {
    let header = if current == 0 { HEADER.len() } else { 0 };
    current.saturating_add((header + row_len) as u64) <= limit
}

/// Moves a full current generation over the previous one.
///
/// Returns `WouldBlock` when another process holds the rotation lock. Once the
/// lock is held the size is read again by path: a generation that has room by
/// now was started by whoever rotated first and is left alone.
fn rotate(path: &Path, row_len: usize, limit: u64) -> io::Result<()> {
    let _lock = RotationLock::acquire(&lock_path(path))?;
    match fs::metadata(path) {
        Ok(current) if !fits(current.len(), row_len, limit) => {
            fs::rename(path, previous_path(path))
        }
        // Rotated by another process, or removed by `clear`, since the caller looked.
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

struct RotationLock(File);

impl RotationLock {
    fn acquire(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Self(file)),
            Err(TryLockError::WouldBlock) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "another process is rotating the debug trace",
            )),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }
}

impl Drop for RotationLock {
    fn drop(&mut self) {
        // Windows releases the lock of a closed handle on its own schedule.
        // Unlocking first lets the next writer in at once.
        let _ = self.0.unlock();
    }
}

/// Keeps a trace that cannot be written from costing I/O on every key.
///
/// After a failed attempt the process stays silent until `quiet_until_ms` on
/// the monotonic clock, so a wall-clock step cannot freeze the trace. The
/// silence doubles per consecutive failure up to [`BACKOFF_MAX_MS`] and carries
/// a per-process jitter, so hosts that failed together do not retry together.
struct Backoff {
    quiet_until_ms: AtomicU64,
    failures: AtomicU32,
}

impl Backoff {
    const fn new() -> Self {
        Self {
            quiet_until_ms: AtomicU64::new(0),
            failures: AtomicU32::new(0),
        }
    }

    fn is_quiet(&self, now_ms: u64) -> bool {
        now_ms < self.quiet_until_ms.load(Ordering::Relaxed)
    }

    fn record_success(&self) {
        if self.failures.load(Ordering::Relaxed) != 0 {
            self.failures.store(0, Ordering::Relaxed);
            self.quiet_until_ms.store(0, Ordering::Relaxed);
        }
    }

    fn record_failure(&self, now_ms: u64, seed: u64) {
        if self.is_quiet(now_ms) {
            // Another thread already started this quiet period.
            return;
        }
        let failures = self.failures.fetch_add(1, Ordering::Relaxed);
        let period = (BACKOFF_FIRST_MS << failures.min(16)).min(BACKOFF_MAX_MS);
        let jitter = seed % (period / 2 + 1);
        self.quiet_until_ms
            .store(now_ms.saturating_add(period + jitter), Ordering::Relaxed);
    }
}

fn monotonic_ms() -> u64 {
    static ANCHOR: OnceLock<Instant> = OnceLock::new();
    u64::try_from(ANCHOR.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Differs between processes, so hosts that fail together back off apart.
fn jitter_seed() -> u64 {
    u64::from(std::process::id()).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32
}

/// The retained trace, oldest row first: the previous generation followed by
/// the current one, with the header shown once. A missing file reads as empty.
pub fn read_text(path: &Path) -> io::Result<String> {
    // The current generation is read first. A rotation between the two reads
    // then repeats rows, where the other order would lose a whole generation.
    let current = read_generation(path)?;
    let previous = read_generation(&previous_path(path))?;
    Ok(join_generations(previous, &current))
}

fn read_generation(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error),
    }
}

fn join_generations(mut previous: String, current: &str) -> String {
    if previous.is_empty() {
        return current.to_owned();
    }
    previous.push_str(current.strip_prefix(HEADER).unwrap_or(current));
    previous
}

/// Removes both generations and the rotation lock.
pub fn clear(path: &Path) -> io::Result<()> {
    // Every file is attempted even when one fails, so a stuck file cannot hide
    // a stale generation. The first failure is the one reported.
    let current = remove_if_present(path);
    let previous = remove_if_present(&previous_path(path));
    let lock = remove_if_present(&lock_path(path));
    current.and(previous).and(lock)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod support {
    use super::*;
    use std::sync::atomic::AtomicU64;

    /// Small enough that a few dozen rows fill a generation.
    pub const LIMIT: u64 = 2048;
    /// Generous upper bound for one row, used for overshoot allowances.
    pub const MAX_ROW_BYTES: u64 = 128;

    static NEXT: AtomicU64 = AtomicU64::new(1);

    /// A private folder per test, removed even when an assertion fails.
    pub struct TestDir(PathBuf);

    impl TestDir {
        pub fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "sakura-debug-trace-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&dir);
            Self(dir)
        }

        pub fn root(&self) -> &Path {
            &self.0
        }

        /// The current generation. Its folder is missing until the first
        /// write, like `logs` on a fresh profile.
        pub fn trace(&self) -> PathBuf {
            self.0.join("logs").join("debug.tsv")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    pub fn record(n: u64) -> TraceEvent {
        TraceEvent {
            component: "tsf",
            instance: 7,
            event: "key_result",
            decision: "commit",
            k0: n,
            k1: 1,
            k2: 2,
            k3: 3,
        }
    }

    /// `k0` of every row in file order. Panics on a row that is not whole.
    pub fn sequence(text: &str) -> Vec<u64> {
        text.lines()
            .filter(|line| *line != HEADER.trim_end_matches('\n'))
            .map(|line| {
                let fields: Vec<&str> = line.split('\t').collect();
                assert_eq!(fields.len(), 11, "torn row: {line:?}");
                fields[7]
                    .parse()
                    .unwrap_or_else(|_| panic!("torn row: {line:?}"))
            })
            .collect()
    }

    pub fn size(path: &Path) -> u64 {
        fs::metadata(path).map_or(0, |metadata| metadata.len())
    }

    /// Stands in for another process that is in the middle of a rotation.
    pub fn hold_rotation_lock(path: &Path) -> File {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create the folder");
        }
        let lock = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(lock_path(path))
            .expect("open the lock file");
        lock.try_lock().expect("take the rotation lock");
        lock
    }

    /// Both generations stay near the ceiling and every row in them is whole.
    pub fn assert_bounded_and_whole(path: &Path, writers: u64) {
        // Rows already in flight when a generation was rotated away can land
        // after its ceiling, one per writer.
        let ceiling = LIMIT + writers * MAX_ROW_BYTES;
        for file in [path.to_path_buf(), previous_path(path)] {
            let len = fs::metadata(&file)
                .unwrap_or_else(|error| panic!("{file:?}: {error}"))
                .len();
            assert!(
                len <= ceiling,
                "{file:?} holds {len} bytes, ceiling {ceiling}"
            );
        }
        // A generation is rotated away only once it is full. A previous
        // generation well below the ceiling means a writer renamed one that
        // another process had just started.
        let previous = size(&previous_path(path));
        assert!(
            previous + MAX_ROW_BYTES > LIMIT,
            "the previous generation holds only {previous} bytes"
        );
        sequence(&read_text(path).expect("read"));
    }
}

#[cfg(test)]
mod tests {
    use super::support::*;
    use super::*;
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    const CHILD_PATH: &str = "SAKURA_DEBUG_TRACE_TEST_PATH";
    const CHILD_BASE: &str = "SAKURA_DEBUG_TRACE_TEST_BASE";
    const CHILD_ROWS: u64 = 300;

    #[test]
    fn emit_at_writes_header_and_one_content_free_row() {
        let dir = TestDir::new();
        let path = dir.trace();
        emit_at(
            &path,
            TraceEvent {
                component: "tsf",
                instance: 7,
                event: "candidate_hide",
                decision: "keep",
                k0: 0,
                k1: 1,
                k2: 0,
                k3: 0,
            },
        )
        .expect("write");
        let text = fs::read_to_string(&path).expect("read");
        assert!(text.starts_with(HEADER));
        assert!(text.contains("\ttsf\t7\tcandidate_hide\tkeep\t0\t1\t0\t0\n"));
        assert!(!text.contains("にほんご"));
    }

    #[test]
    fn disabled_emit_is_a_noop() {
        set_enabled(false);
        emit(TraceEvent {
            component: "tsf",
            instance: 1,
            event: "conversion_key",
            decision: "absorb_peer",
            k0: 0,
            k1: 1,
            k2: 2,
            k3: 0,
        });
    }

    #[test]
    fn a_full_trace_rotates_and_keeps_the_newest_rows() {
        let dir = TestDir::new();
        let path = dir.trace();
        for n in 0..300 {
            write_record(&path, record(n), LIMIT).expect("write");
        }
        let text = read_text(&path).expect("read");
        let rows = sequence(&text);
        assert_eq!(rows.last(), Some(&299), "the newest row is kept");
        assert!(
            rows.windows(2).all(|pair| pair[1] == pair[0] + 1),
            "one unbroken run, no gap or repeat: {rows:?}"
        );
        // At least one whole generation of rows survives the rotation.
        assert!(rows.len() as u64 >= (LIMIT - HEADER.len() as u64) / MAX_ROW_BYTES);
        assert_eq!(text.matches(HEADER).count(), 1, "the header is shown once");
        assert!(size(&path) <= LIMIT && size(&previous_path(&path)) <= LIMIT);
    }

    #[test]
    fn a_trace_beyond_the_ceiling_is_rotated_whole_not_dropped() {
        let dir = TestDir::new();
        let path = dir.trace();
        // A file written by a build whose ceiling was larger than this one.
        for n in 0..80 {
            write_record(&path, record(n), u64::MAX).expect("legacy row");
        }
        let legacy = fs::read(&path).expect("legacy file");
        assert!(legacy.len() as u64 > LIMIT);
        write_record(&path, record(80), LIMIT).expect("first row past the ceiling");
        assert_eq!(
            fs::read(previous_path(&path)).expect("previous"),
            legacy,
            "the oversized file moved over whole"
        );
        assert_eq!(
            sequence(&read_text(&path).expect("read")),
            (0..=80).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_rotation_in_progress_drops_the_row_and_touches_nothing() {
        let dir = TestDir::new();
        let path = dir.trace();
        for n in 0..20 {
            write_record(&path, record(n), LIMIT).expect("write");
        }
        let before = fs::read(&path).expect("read");
        // The file's own size as the ceiling: no further row fits.
        let full = before.len() as u64;
        let holder = hold_rotation_lock(&path);
        let error = write_record(&path, record(20), full).expect_err("another process rotates");
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(
            fs::read(&path).expect("read"),
            before,
            "the current generation is untouched"
        );
        assert!(!previous_path(&path).exists(), "nothing was rotated");

        holder.unlock().expect("release");
        drop(holder);
        write_record(&path, record(20), full).expect("rotates once the lock is free");
        assert_eq!(fs::read(previous_path(&path)).expect("previous"), before);
        assert_eq!(
            sequence(&read_text(&path).expect("read")),
            (0..=20).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_failed_trace_stays_silent_without_touching_the_disk() {
        let dir = TestDir::new();
        let path = dir.trace();
        for n in 0..20 {
            write_record(&path, record(n), LIMIT).expect("write");
        }
        let full = size(&path);
        let backoff = Backoff::new();
        let holder = hold_rotation_lock(&path);
        assert!(
            emit_through(&backoff, 1_000, 0, &path, record(20), full).is_err(),
            "the first attempt is made, and fails"
        );
        holder.unlock().expect("release");
        drop(holder);

        // With the folder gone, any further access would bring it back.
        fs::remove_dir_all(dir.root().join("logs")).expect("remove the folder");
        for step in 0..1_000 {
            let outcome = emit_through(&backoff, 1_001 + step % 90, 0, &path, record(21), full);
            assert!(outcome.is_err(), "silent during the quiet period");
        }
        assert!(
            !dir.root().join("logs").exists(),
            "a silent trace must not touch the disk"
        );

        emit_through(&backoff, 5_000, 0, &path, record(22), full)
            .expect("writes again once the quiet period is over");
        assert_eq!(sequence(&read_text(&path).expect("read")), vec![22]);
    }

    #[test]
    fn a_refused_rename_keeps_the_data_and_reports_the_failure() {
        let dir = TestDir::new();
        let path = dir.trace();
        for n in 0..20 {
            write_record(&path, record(n), LIMIT).expect("write");
        }
        let before = fs::read(&path).expect("read");
        // A directory with content cannot be replaced by a file.
        let blocker = previous_path(&path);
        fs::create_dir_all(blocker.join("inside")).expect("blocker");
        let outcome = write_record(&path, record(20), before.len() as u64);
        assert!(outcome.is_err(), "the rotation cannot happen");
        assert_eq!(fs::read(&path).expect("read"), before, "no row was lost");
    }

    #[test]
    fn a_reader_that_blocks_renames_stops_rotation_but_loses_no_data() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = TestDir::new();
        let path = dir.trace();
        for n in 0..20 {
            write_record(&path, record(n), LIMIT).expect("write");
        }
        let before = fs::read(&path).expect("read");
        let full = before.len() as u64;
        // A tool following the file with FILE_SHARE_READ | FILE_SHARE_WRITE.
        let follower = OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x2)
            .open(&path)
            .expect("follower");
        assert!(
            write_record(&path, record(20), full).is_err(),
            "the full trace cannot rotate under the follower"
        );
        drop(follower);
        assert_eq!(fs::read(&path).expect("read"), before);
        assert!(!previous_path(&path).exists());
        write_record(&path, record(20), full).expect("rotates once the follower lets go");
    }

    #[test]
    fn concurrent_writers_keep_every_row_whole_and_the_trace_bounded() {
        const THREADS: u64 = 8;
        const ROWS: u64 = 400;
        let dir = TestDir::new();
        let path = dir.trace();
        std::thread::scope(|scope| {
            for thread in 0..THREADS {
                let path = &path;
                scope.spawn(move || {
                    for n in 0..ROWS {
                        // Rows lost to a rotation in progress are expected.
                        let _ = write_record(path, record(thread * 10_000 + n), LIMIT);
                    }
                });
            }
        });
        assert!(previous_path(&path).exists(), "the writers rotated");
        assert_bounded_and_whole(&path, THREADS);
        write_record(&path, record(999_999), LIMIT).expect("the trace is not wedged");
        assert_eq!(
            sequence(&read_text(&path).expect("read")).last(),
            Some(&999_999)
        );
    }

    /// Body of one writer process for the test below. A normal run has no
    /// environment for it and returns at once.
    #[test]
    fn child_writer() {
        let (Some(path), Some(base)) = (std::env::var_os(CHILD_PATH), std::env::var_os(CHILD_BASE))
        else {
            return;
        };
        let base: u64 = base.to_string_lossy().parse().expect("row base");
        let path = PathBuf::from(path);
        for n in 0..CHILD_ROWS {
            let _ = write_record(&path, record(base + n), LIMIT);
        }
    }

    #[test]
    fn writers_in_separate_processes_rotate_without_losing_the_trace() {
        const WRITERS: u64 = 4;
        let dir = TestDir::new();
        let path = dir.trace();
        let exe = std::env::current_exe().expect("test binary");
        let mut children: Vec<Child> = (0..WRITERS)
            .map(|index| {
                Command::new(&exe)
                    .args(["--exact", "debug_trace::tests::child_writer"])
                    .env(CHILD_PATH, &path)
                    .env(CHILD_BASE, (index * 10_000).to_string())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("spawn a writer process")
            })
            .collect();
        wait_for_all(&mut children, Duration::from_secs(60));
        assert!(previous_path(&path).exists(), "the writers rotated");
        assert_bounded_and_whole(&path, WRITERS);
        write_record(&path, record(999_999), LIMIT).expect("the trace is not wedged");
        assert_eq!(
            sequence(&read_text(&path).expect("read")).last(),
            Some(&999_999)
        );
    }

    fn wait_for_all(children: &mut [Child], deadline: Duration) {
        let started = Instant::now();
        for index in 0..children.len() {
            loop {
                if let Some(status) = children[index].try_wait().expect("poll a writer") {
                    assert!(status.success(), "writer process failed: {status}");
                    break;
                }
                if started.elapsed() > deadline {
                    for child in children.iter_mut() {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    panic!("writer processes did not finish within {deadline:?}");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    #[test]
    fn clear_removes_every_file_the_trace_owns() {
        let dir = TestDir::new();
        let path = dir.trace();
        for n in 0..200 {
            write_record(&path, record(n), LIMIT).expect("write");
        }
        assert!(previous_path(&path).exists() && lock_path(&path).exists());
        clear(&path).expect("clear");
        assert!(!path.exists() && !previous_path(&path).exists() && !lock_path(&path).exists());
        assert_eq!(read_text(&path).expect("read"), "");
        clear(&path).expect("clearing an empty trace is fine");
    }
}

#[cfg(test)]
mod internals {
    use super::support::*;
    use super::*;

    #[test]
    fn a_rotator_that_lost_a_race_leaves_the_fresh_generation_alone() {
        let dir = TestDir::new();
        let path = dir.trace();
        for n in 0..20 {
            write_record(&path, record(n), LIMIT).expect("write");
        }
        let old = fs::read(&path).expect("read");
        let full = old.len() as u64;
        // Another process finishes the rotation this one was about to start.
        write_record(&path, record(20), full).expect("rotate and append");
        let fresh = fs::read(&path).expect("fresh generation");
        // This one looked before that happened. Its rotation finds room.
        rotate(&path, 64, full).expect("nothing to rotate");
        assert_eq!(fs::read(&path).expect("read"), fresh);
        assert_eq!(fs::read(previous_path(&path)).expect("previous"), old);
    }

    #[test]
    fn backoff_doubles_to_a_ceiling_and_a_success_resets_it() {
        let backoff = Backoff::new();
        assert!(!backoff.is_quiet(0));
        let mut now = 1_000;
        let mut period = BACKOFF_FIRST_MS;
        for _ in 0..20 {
            backoff.record_failure(now, 0);
            assert!(backoff.is_quiet(now + period - 1), "quiet for {period} ms");
            assert!(
                !backoff.is_quiet(now + period),
                "not quiet after {period} ms"
            );
            now += period;
            period = (period * 2).min(BACKOFF_MAX_MS);
        }
        assert_eq!(period, BACKOFF_MAX_MS, "the period stops growing");

        backoff.record_success();
        assert!(!backoff.is_quiet(now));
        backoff.record_failure(now, 0);
        assert!(backoff.is_quiet(now + BACKOFF_FIRST_MS - 1));
        assert!(
            !backoff.is_quiet(now + BACKOFF_FIRST_MS),
            "a success starts the schedule over"
        );
    }

    #[test]
    fn a_failure_inside_a_quiet_period_does_not_escalate_it() {
        let backoff = Backoff::new();
        backoff.record_failure(1_000, 0);
        backoff.record_failure(1_010, 0);
        backoff.record_failure(1_099, 0);
        assert!(!backoff.is_quiet(1_000 + BACKOFF_FIRST_MS));
        backoff.record_failure(1_100, 0);
        assert!(
            !backoff.is_quiet(1_100 + 2 * BACKOFF_FIRST_MS),
            "the next period is the second step, not the fourth"
        );
        assert!(backoff.is_quiet(1_100 + 2 * BACKOFF_FIRST_MS - 1));
    }

    #[test]
    fn backoff_jitter_stays_within_half_the_period() {
        for seed in [0, 1, 49, 50, 51, 12_345, u64::MAX] {
            let backoff = Backoff::new();
            backoff.record_failure(0, seed);
            assert!(backoff.is_quiet(BACKOFF_FIRST_MS - 1), "seed {seed}");
            assert!(
                !backoff.is_quiet(BACKOFF_FIRST_MS + BACKOFF_FIRST_MS / 2),
                "seed {seed}"
            );
        }
    }

    #[test]
    fn generations_join_under_one_header() {
        let old = format!("{HEADER}1\n");
        let new = format!("{HEADER}2\n");
        assert_eq!(
            join_generations(old.clone(), &new),
            format!("{HEADER}1\n2\n")
        );
        assert_eq!(join_generations(String::new(), &new), new);
        assert_eq!(join_generations(old.clone(), ""), old);
        assert_eq!(join_generations(old, "2\n"), format!("{HEADER}1\n2\n"));
    }
}
