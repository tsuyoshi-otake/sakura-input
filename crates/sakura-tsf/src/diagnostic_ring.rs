//! Fixed-capacity, opt-in metadata diagnostics for TSF crash attribution.
//!
//! The ring is intentionally independent of the file/registry diagnostics
//! paths. Recording is a handful of atomic stores: it never allocates, waits
//! on a lock, performs I/O, or observes text. A minidump may omit this memory;
//! callers must not assume that a WER dump contains a readable ring.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use windows::Win32::System::Threading::GetCurrentThreadId;

pub const SCHEMA_VERSION: u16 = 1;
pub const CAPACITY: usize = 64;
pub const BUILD_IDENTITY: u64 = build_identity(env!("CARGO_PKG_VERSION"));

const WORDS: usize = 9;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum RequestKind {
    None = 0,
    KeyWrite = 1,
    Commit = 2,
    Reconvert = 3,
    Candidate = 4,
    Layout = 5,
    Lifecycle = 6,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestPath {
    None = 0,
    Sync = 1,
    Async = 2,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalOutcome {
    None = 0,
    Admitted = 1,
    Applied = 2,
    Rejected = 3,
    Cancelled = 4,
    Unknown = 5,
    Deferred = 6,
    Failed = 7,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleEvent {
    None = 0,
    Activate = 1,
    Deactivate = 2,
    FocusChanged = 3,
    CompositionStarted = 4,
    CompositionEnded = 5,
    ContextReplaced = 6,
}

/// Metadata accepted by the ring. There is deliberately no string or byte
/// field, so text cannot be recorded accidentally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metadata {
    pub context_identity: u64,
    pub focus_generation: u64,
    pub document_revision: u64,
    pub composition_generation: u64,
    pub write_ticket: u64,
    pub request_kind: RequestKind,
    pub path: RequestPath,
    pub outcome: TerminalOutcome,
    pub error_code: i32,
    pub lifecycle: LifecycleEvent,
}

impl Metadata {
    pub const fn lifecycle(event: LifecycleEvent, context_identity: u64) -> Self {
        Self {
            context_identity,
            focus_generation: 0,
            document_revision: 0,
            composition_generation: 0,
            write_ticket: 0,
            request_kind: RequestKind::Lifecycle,
            path: RequestPath::None,
            outcome: TerminalOutcome::None,
            error_code: 0,
            lifecycle: event,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub const fn request(
        context_identity: u64,
        focus_generation: u64,
        document_revision: u64,
        composition_generation: u64,
        write_ticket: u64,
        request_kind: RequestKind,
        path: RequestPath,
        outcome: TerminalOutcome,
        error_code: i32,
    ) -> Self {
        Self {
            context_identity,
            focus_generation,
            document_revision,
            composition_generation,
            write_ticket,
            request_kind,
            path,
            outcome,
            error_code,
            lifecycle: LifecycleEvent::None,
        }
    }
}

/// A decoded event used by diagnostic tests and a future debugger/exporter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub schema_version: u16,
    pub build_identity: u64,
    pub sequence: u64,
    pub thread_id: u32,
    pub context_identity: u64,
    pub focus_generation: u64,
    pub document_revision: u64,
    pub composition_generation: u64,
    pub write_ticket: u64,
    pub request_kind: RequestKind,
    pub path: RequestPath,
    pub outcome: TerminalOutcome,
    pub error_code: i32,
    pub lifecycle: LifecycleEvent,
}

struct Slot {
    sequence: AtomicU64,
    words: [AtomicU64; WORDS],
}

impl Slot {
    const fn new() -> Self {
        Self {
            sequence: AtomicU64::new(0),
            words: [const { AtomicU64::new(0) }; WORDS],
        }
    }
}

struct Ring {
    enabled: AtomicBool,
    next: AtomicU64,
    slots: [Slot; CAPACITY],
}

impl Ring {
    const fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            next: AtomicU64::new(0),
            slots: [const { Slot::new() }; CAPACITY],
        }
    }

    fn record(&self, metadata: Metadata) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        let ticket = self.next.fetch_add(1, Ordering::Relaxed);
        let sequence = ticket.saturating_add(1);
        let Some(slot) = self.slots.get((ticket as usize) % CAPACITY) else {
            return;
        };
        let words = pack(metadata, sequence);
        for (index, word) in words.into_iter().enumerate() {
            if let Some(cell) = slot.words.get(index) {
                cell.store(word, Ordering::Relaxed);
            }
        }
        slot.sequence.store(sequence, Ordering::Release);
    }
}

static RING: Ring = Ring::new();

/// Enables or disables the metadata ring. This is called from activation or a
/// developer-only test harness, never from a key handler.
pub fn set_enabled(enabled: bool) {
    RING.enabled.store(enabled, Ordering::Release);
}

/// Reads the opt-in bit without touching any event fields. Callers use this to
/// keep disabled key paths to a single relaxed atomic load.
pub fn is_enabled() -> bool {
    RING.enabled.load(Ordering::Relaxed)
}

/// Reads the explicit developer opt-in without touching the key path.
pub fn initialize_from_environment() {
    let enabled = std::env::var_os("SAKURA_TSF_DIAGNOSTICS")
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("on"));
    set_enabled(enabled);
}

/// Emits one metadata event. When disabled, this is one relaxed atomic load.
pub fn record(metadata: Metadata) {
    RING.record(metadata);
}

/// Copies a stable best-effort snapshot into caller-owned fixed storage. The
/// function does not allocate and is intended for tests/debuggers, not typing.
pub fn snapshot(output: &mut [Event; CAPACITY]) -> usize {
    let mut count = 0usize;
    for slot in &RING.slots {
        let first = slot.sequence.load(Ordering::Acquire);
        if first == 0 {
            continue;
        }
        let mut words = [0u64; WORDS];
        for (index, word) in words.iter_mut().enumerate() {
            if let Some(cell) = slot.words.get(index) {
                *word = cell.load(Ordering::Relaxed);
            }
        }
        let second = slot.sequence.load(Ordering::Acquire);
        if first != second {
            continue;
        }
        if let Some(event) = unpack(words) {
            if count < output.len() {
                if let Some(slot) = output.get_mut(count) {
                    *slot = event;
                    count += 1;
                }
            }
        }
    }
    if let Some(events) = output.get_mut(..count) {
        events.sort_unstable_by_key(|event| event.sequence);
    }
    count
}

fn pack(metadata: Metadata, sequence: u64) -> [u64; WORDS] {
    let header = SCHEMA_VERSION as u64
        | ((metadata.request_kind as u64) << 16)
        | ((metadata.path as u64) << 24)
        | ((metadata.outcome as u64) << 32)
        | ((metadata.lifecycle as u64) << 40);
    [
        header,
        BUILD_IDENTITY,
        sequence,
        {
            // SAFETY: GetCurrentThreadId has no preconditions and does not
            // access any caller-provided memory.
            unsafe { GetCurrentThreadId() }
        } as u64
            | ((metadata.error_code as u32 as u64) << 32),
        metadata.context_identity,
        metadata.focus_generation,
        metadata.document_revision,
        metadata.composition_generation,
        metadata.write_ticket,
    ]
}

fn unpack(words: [u64; WORDS]) -> Option<Event> {
    let schema_version = (words[0] & 0xffff) as u16;
    let request_kind = match ((words[0] >> 16) & 0xff) as u8 {
        0 => RequestKind::None,
        1 => RequestKind::KeyWrite,
        2 => RequestKind::Commit,
        3 => RequestKind::Reconvert,
        4 => RequestKind::Candidate,
        5 => RequestKind::Layout,
        6 => RequestKind::Lifecycle,
        _ => return None,
    };
    let path = match ((words[0] >> 24) & 0xff) as u8 {
        0 => RequestPath::None,
        1 => RequestPath::Sync,
        2 => RequestPath::Async,
        _ => return None,
    };
    let outcome = match ((words[0] >> 32) & 0xff) as u8 {
        0 => TerminalOutcome::None,
        1 => TerminalOutcome::Admitted,
        2 => TerminalOutcome::Applied,
        3 => TerminalOutcome::Rejected,
        4 => TerminalOutcome::Cancelled,
        5 => TerminalOutcome::Unknown,
        6 => TerminalOutcome::Deferred,
        7 => TerminalOutcome::Failed,
        _ => return None,
    };
    let lifecycle = match ((words[0] >> 40) & 0xff) as u8 {
        0 => LifecycleEvent::None,
        1 => LifecycleEvent::Activate,
        2 => LifecycleEvent::Deactivate,
        3 => LifecycleEvent::FocusChanged,
        4 => LifecycleEvent::CompositionStarted,
        5 => LifecycleEvent::CompositionEnded,
        6 => LifecycleEvent::ContextReplaced,
        _ => return None,
    };
    Some(Event {
        schema_version,
        build_identity: words[1],
        sequence: words[2],
        thread_id: words[3] as u32,
        context_identity: words[4],
        focus_generation: words[5],
        document_revision: words[6],
        composition_generation: words[7],
        write_ticket: words[8],
        request_kind,
        path,
        outcome,
        error_code: (words[3] >> 32) as u32 as i32,
        lifecycle,
    })
}

const fn build_identity(version: &str) -> u64 {
    let bytes = version.as_bytes();
    let (major, index) = version_component(bytes, 0);
    let (minor, index) = version_component(bytes, index.saturating_add(1));
    let (patch, _) = version_component(bytes, index.saturating_add(1));
    (major << 32) | (minor << 16) | patch
}

#[allow(clippy::indexing_slicing)]
const fn version_component(bytes: &[u8], mut index: usize) -> (u64, usize) {
    let mut value = 0u64;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte < b'0' || byte > b'9' {
            break;
        }
        value = value
            .saturating_mul(10)
            .saturating_add((byte - b'0') as u64);
        index += 1;
    }
    (value, index)
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use super::*;

    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn reset() {
        set_enabled(false);
        RING.next.store(0, Ordering::Relaxed);
        for slot in &RING.slots {
            slot.sequence.store(0, Ordering::Relaxed);
            for word in &slot.words {
                word.store(0, Ordering::Relaxed);
            }
        }
    }

    fn event() -> Metadata {
        Metadata::request(
            0x11,
            2,
            3,
            4,
            5,
            RequestKind::KeyWrite,
            RequestPath::Async,
            TerminalOutcome::Applied,
            -2147467259,
        )
    }

    #[test]
    fn disabled_ring_is_a_noop() {
        let _guard = test_guard();
        reset();
        let mut output = [Event {
            schema_version: 0,
            build_identity: 0,
            sequence: 0,
            thread_id: 0,
            context_identity: 0,
            focus_generation: 0,
            document_revision: 0,
            composition_generation: 0,
            write_ticket: 0,
            request_kind: RequestKind::None,
            path: RequestPath::None,
            outcome: TerminalOutcome::None,
            error_code: 0,
            lifecycle: LifecycleEvent::None,
        }; CAPACITY];
        record(event());
        assert_eq!(snapshot(&mut output), 0);
    }

    #[test]
    fn redaction_surface_has_no_text_bearing_fields() {
        let _guard = test_guard();
        reset();
        let metadata = event();
        let _ = metadata;
        assert_eq!(std::mem::size_of::<Metadata>(), 48);
        assert!(!std::mem::needs_drop::<Metadata>());
        assert!(!std::mem::needs_drop::<Event>());
    }

    #[test]
    fn enabled_ring_roundtrips_classified_metadata() {
        let _guard = test_guard();
        reset();
        set_enabled(true);
        record(event());
        let mut output = [Event {
            schema_version: 0,
            build_identity: 0,
            sequence: 0,
            thread_id: 0,
            context_identity: 0,
            focus_generation: 0,
            document_revision: 0,
            composition_generation: 0,
            write_ticket: 0,
            request_kind: RequestKind::None,
            path: RequestPath::None,
            outcome: TerminalOutcome::None,
            error_code: 0,
            lifecycle: LifecycleEvent::None,
        }; CAPACITY];
        let count = snapshot(&mut output);
        assert!(count >= 1);
        let last = output
            .get(count.saturating_sub(1))
            .copied()
            .unwrap_or(Event {
                schema_version: 0,
                build_identity: 0,
                sequence: 0,
                thread_id: 0,
                context_identity: 0,
                focus_generation: 0,
                document_revision: 0,
                composition_generation: 0,
                write_ticket: 0,
                request_kind: RequestKind::None,
                path: RequestPath::None,
                outcome: TerminalOutcome::None,
                error_code: 0,
                lifecycle: LifecycleEvent::None,
            });
        assert_eq!(last.schema_version, SCHEMA_VERSION);
        assert_eq!(last.build_identity, BUILD_IDENTITY);
        assert_eq!(last.context_identity, 0x11);
        assert_eq!(last.focus_generation, 2);
        assert_eq!(last.document_revision, 3);
        assert_eq!(last.composition_generation, 4);
        assert_eq!(last.write_ticket, 5);
        assert_eq!(last.request_kind, RequestKind::KeyWrite);
        assert_eq!(last.path, RequestPath::Async);
        assert_eq!(last.outcome, TerminalOutcome::Applied);
        assert_eq!(last.error_code, -2147467259);
        set_enabled(false);
    }

    #[test]
    fn wraparound_keeps_only_the_fixed_capacity() {
        let _guard = test_guard();
        reset();
        set_enabled(true);
        for _ in 0..(CAPACITY + 7) {
            record(event());
        }
        let mut output = [Event {
            schema_version: 0,
            build_identity: 0,
            sequence: 0,
            thread_id: 0,
            context_identity: 0,
            focus_generation: 0,
            document_revision: 0,
            composition_generation: 0,
            write_ticket: 0,
            request_kind: RequestKind::None,
            path: RequestPath::None,
            outcome: TerminalOutcome::None,
            error_code: 0,
            lifecycle: LifecycleEvent::None,
        }; CAPACITY];
        assert_eq!(snapshot(&mut output), CAPACITY);
        let first = output.first().map(|event| event.sequence).unwrap_or(0);
        let last = output.last().map(|event| event.sequence).unwrap_or(0);
        assert!(first < last);
        set_enabled(false);
    }

    #[test]
    fn lifecycle_event_roundtrips_without_request_text() {
        let _guard = test_guard();
        reset();
        set_enabled(true);
        record(Metadata::lifecycle(LifecycleEvent::FocusChanged, 99));
        let mut output = [Event {
            schema_version: 0,
            build_identity: 0,
            sequence: 0,
            thread_id: 0,
            context_identity: 0,
            focus_generation: 0,
            document_revision: 0,
            composition_generation: 0,
            write_ticket: 0,
            request_kind: RequestKind::None,
            path: RequestPath::None,
            outcome: TerminalOutcome::None,
            error_code: 0,
            lifecycle: LifecycleEvent::None,
        }; CAPACITY];
        let count = snapshot(&mut output);
        let last = output
            .get(count.saturating_sub(1))
            .copied()
            .unwrap_or(Event {
                schema_version: 0,
                build_identity: 0,
                sequence: 0,
                thread_id: 0,
                context_identity: 0,
                focus_generation: 0,
                document_revision: 0,
                composition_generation: 0,
                write_ticket: 0,
                request_kind: RequestKind::None,
                path: RequestPath::None,
                outcome: TerminalOutcome::None,
                error_code: 0,
                lifecycle: LifecycleEvent::None,
            });
        assert_eq!(last.lifecycle, LifecycleEvent::FocusChanged);
        assert_eq!(last.context_identity, 99);
        assert_eq!(last.request_kind, RequestKind::Lifecycle);
        set_enabled(false);
    }
}
