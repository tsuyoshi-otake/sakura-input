//! Zero-allocation assertions for the engine's hot path (DESIGN.md §5.7,
//! §10).
//!
//! This is the one place in the crate that uses `unsafe`: proving "does
//! not allocate" from safe code alone is not possible on stable Rust
//! without an external crate (disallowed by this crate's zero-dependency
//! policy), so this file installs a counting `#[global_allocator]`
//! wrapper around `std::alloc::System`. `GlobalAlloc` is an `unsafe
//! trait`, so implementing it is unavoidably `unsafe impl`; every method
//! here does nothing but count the call and forward unchanged to
//! `System`, whose own implementation supplies the actual safety
//! guarantee. The library crate itself (`src/`) remains free of
//! `unsafe`.
//!
//! Deliberately a single `#[test]` function: `cargo test` runs multiple
//! tests concurrently on separate threads by default, and spawning a
//! second test's thread is itself an allocation (the harness boxes the
//! test closure and its thread state) that a shared global counter would
//! see -- including inside another, unrelated test's measurement window.
//! One test means the harness has nothing else to schedule while this one
//! measures, so the counter only ever reflects this file's own code.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use sakura_proto::{
    decode_request, encode_request, KeyCode, KeyInput, Modifiers, OutputBuf, Request,
    UnderlineKind, FRAME_HEADER_LEN,
};

static ALLOC_CALLS: AtomicUsize = AtomicUsize::new(0);

/// Forwards every call to `System`, counting `alloc`/`alloc_zeroed`/
/// `realloc` calls on the way through.
struct CountingAllocator;

// SAFETY: every method here does nothing but bump a counter and forward
// its arguments unchanged to the corresponding `System` method; `System`
// is itself a correct `GlobalAlloc` implementation, and this wrapper
// changes no allocation behavior (sizes, alignment, pointer provenance).
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_CALLS.fetch_add(1, Ordering::SeqCst);
        // SAFETY: `layout` is forwarded unchanged; the caller of this
        // method is bound by `GlobalAlloc::alloc`'s own contract, which is
        // identical to `System::alloc`'s.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr`/`layout` are forwarded unchanged; the caller is
        // bound by `GlobalAlloc::dealloc`'s contract (matching allocation
        // from this same allocator), which is identical to `System`'s.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOC_CALLS.fetch_add(1, Ordering::SeqCst);
        // SAFETY: arguments forwarded unchanged; same contract as
        // `System::realloc`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOC_CALLS.fetch_add(1, Ordering::SeqCst);
        // SAFETY: `layout` forwarded unchanged; same contract as
        // `System::alloc_zeroed`.
        unsafe { System.alloc_zeroed(layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

fn alloc_calls() -> usize {
    ALLOC_CALLS.load(Ordering::SeqCst)
}

/// Runs the engine's per-keystroke hot path once: decode a `SendKey`
/// request (no `String`/`Vec` fields, so nothing to allocate), build an
/// `OutputBuf` with one segment and a cursor, and encode it into a
/// preallocated buffer.
fn run_hot_path(payload: &[u8], buf: &mut OutputBuf, out_frame: &mut [u8]) {
    let (_id, decoded) = decode_request(payload).expect("decode SendKey");
    match decoded {
        Request::SendKey { .. } => {}
        other => panic!("expected SendKey, got {other:?}"),
    }
    buf.clear();
    buf.begin_preedit();
    buf.push_segment("あ", UnderlineKind::Raw)
        .expect("push_segment");
    buf.set_cursor(1);
    let n = buf.encode_frame(1, out_frame).expect("encode_frame");
    assert!(n > 0);
}

#[test]
fn zero_allocation_hot_path_and_output_buf_new() {
    // `OutputBuf::new` must not allocate: every field is a fixed-size
    // stack buffer. Measured first, on its own, before anything else in
    // this test has touched the allocator counter.
    let before = alloc_calls();
    let fresh = OutputBuf::new();
    let after = alloc_calls();
    assert_eq!(after, before, "OutputBuf::new allocated");
    assert_eq!(fresh.preedit_text(), "");

    // Hot path: decode -> build -> encode. Setup (building the encoded
    // request bytes, allocating the reusable buffers) happens before
    // measurement and may allocate freely.
    let key = KeyInput {
        code: KeyCode::Char,
        ch: Some('a'),
        modifiers: Modifiers::SHIFT,
        repeat: false,
        test_only: false,
    };
    let request = Request::SendKey { session: 7, key };
    let mut encoded = Vec::new();
    encode_request(&request, 1, &mut encoded).expect("encode");
    let payload = encoded[FRAME_HEADER_LEN..].to_vec();
    let mut out_frame = vec![0u8; 512];
    let mut buf = OutputBuf::new();

    // Warm-up: absorb any first-use lazy initialization (allocator
    // internals, TLS, etc.) that is not part of the steady-state cost.
    run_hot_path(&payload, &mut buf, &mut out_frame);

    let before = alloc_calls();
    run_hot_path(&payload, &mut buf, &mut out_frame);
    let after = alloc_calls();

    assert_eq!(
        after,
        before,
        "hot path allocated {} time(s)",
        after - before
    );
}
