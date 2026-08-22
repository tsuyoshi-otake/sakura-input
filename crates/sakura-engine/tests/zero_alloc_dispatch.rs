//! The hot path allocates nothing (DESIGN 5.7), asserted rather than
//! claimed — this is [`sakura_core`'s zero-allocation test][core], one layer
//! up: it proves the promise still holds once romaji, key resolution, and
//! width normalization are wired together behind [`Dispatcher::dispatch`]
//! and driven entirely through the wire-protocol [`Request`]/[`Response`]
//! shapes a real connection would use.
//!
//! [core]: https://docs.rs/sakura-core (see `crates/sakura-core/tests/zero_alloc.rs`)
//!
//! `CreateSession` is allowed to allocate — it happens once per connection,
//! not per keystroke, and its `process_name` field is a `String` in the wire
//! protocol. What must not allocate is either legacy `SendKey(test_only)` or
//! the scope-carrying `ProbeKey`, both run against an `OutputBuf` the caller
//! reuses across keystrokes, which is exactly what a pipe worker thread does
//! for the lifetime of a connection.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use dictc::{compile, parse_connection, parse_entries};
use sakura_core::{
    ConversionInput, ConversionOptions, CrossCommitBridge, Preferences, RightContextId,
};
use sakura_engine::dictionary::ConversionService;
use sakura_engine::dispatch::{Dispatcher, Reply};
use sakura_engine::learning::LearningService;
use sakura_engine::prediction::PredictionRuntime;
use sakura_proto::{InputScope, KeyCode, KeyInput, Modifiers, OutputBuf, Request, Response};

thread_local! {
    /// `const` initialization matters: a lazily-initialized thread-local
    /// would allocate the first time the allocator touched it, which is a
    /// loop that ends badly.
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

/// Wraps the system allocator and counts every request that hands back new
/// memory. `dealloc` is not counted — freeing is not what costs a keystroke.
struct Counting;

// SAFETY: every method forwards its arguments unchanged to `System`, which
// is a valid `GlobalAlloc`, and returns its result unchanged. The counter is
// a `Cell<usize>` in thread-local storage: it touches no heap memory, so it
// cannot re-enter the allocator.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: `layout` is forwarded exactly as the caller supplied it.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: as above.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        // SAFETY: `ptr` and `layout` come from a previous `alloc` by this
        // allocator, which is `System`'s, and are forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: as above.
        unsafe { System.dealloc(ptr, layout) }
    }
}

fn count() {
    // `try_with` rather than `with`: during thread teardown the thread-local
    // is gone, and panicking inside the allocator would abort the process.
    let _ = ALLOCATIONS.try_with(|cell| cell.set(cell.get() + 1));
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Runs `body` and returns how many allocations it made on this thread.
fn allocations(body: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.with(Cell::get);
    body();
    ALLOCATIONS.with(Cell::get) - before
}

fn char_key(c: char) -> KeyInput {
    KeyInput {
        code: KeyCode::Char,
        ch: Some(c),
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    }
}

/// Proves the counter actually sees allocations, so a zero elsewhere means
/// "did not allocate" rather than "counter is broken".
#[test]
fn the_counter_counts() {
    let mut sink = String::new();
    let observed = allocations(|| {
        sink.push_str("this string has to come from somewhere");
    });
    assert!(observed > 0, "a growing String must allocate");
}

/// A whole `SendKey` dispatch, through the same [`Dispatcher`] and the same
/// [`OutputBuf`] a pipe worker reuses for every keystroke of a connection,
/// allocates nothing — even though the request and reply travel through the
/// wire-protocol [`Request`]/[`Reply`] shapes rather than calling the romaji
/// FSM or the width normalizer directly.
#[test]
fn send_key_dispatch_into_a_reused_output_buf_allocates_nothing() {
    let mut dispatcher = Dispatcher::new().expect("the shipped defaults must compile");
    let mut out = OutputBuf::new();

    // Creating the session is allowed to allocate (`process_name` is a
    // `String` in the wire protocol, and this happens once per connection,
    // not per keystroke) — do it, and the warm-up keystrokes below, outside
    // the measured region.
    let session = match dispatcher.dispatch(
        &Request::CreateSession {
            process_name: "warmup.exe".to_string(),
        },
        &mut out,
    ) {
        Reply::Message(Response::SessionCreated { session, .. }) => session,
        other => panic!("expected SessionCreated, got {other:?}"),
    };

    for c in "warmup".chars() {
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key(c),
            },
            &mut out,
        );
    }
    dispatcher.dispatch(&Request::Revert { session }, &mut out);
    out.clear();

    let observed = allocations(|| {
        for c in "konnnichiha".chars() {
            let reply = dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: char_key(c),
                },
                &mut out,
            );
            assert_eq!(
                reply,
                Reply::Output,
                "SendKey must answer via the OutputBuf"
            );
        }
    });

    assert_eq!(observed, 0, "SendKey dispatch allocated");
    assert_eq!(out.preedit_text(), "こんにちは");
}

/// `OnTestKeyDown` is part of the physical-key hot path too. Both the legacy
/// `SendKey(test_only)` and the scope-carrying `ProbeKey` Probe views must not
/// clone the fixed prediction result (or allocate any other temporary cache)
/// merely to answer the host's consumption question.
#[test]
fn test_only_probe_dispatch_into_a_reused_output_buf_allocates_nothing() {
    let mut dispatcher = Dispatcher::new().expect("the shipped defaults must compile");
    let mut out = OutputBuf::new();
    let session = match dispatcher.dispatch(
        &Request::CreateSession {
            process_name: "test-only-alloc.exe".to_owned(),
        },
        &mut out,
    ) {
        Reply::Message(Response::SessionCreated { session, .. }) => session,
        other => panic!("expected SessionCreated, got {other:?}"),
    };

    for character in "warmup".chars() {
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key(character),
            },
            &mut out,
        );
    }
    dispatcher.dispatch(&Request::Revert { session }, &mut out);
    out.clear();

    let probe_key = KeyInput {
        code: KeyCode::Char,
        ch: Some('z'),
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: true,
    };
    let mut legacy_probe_consumed = false;
    let mut fresh_probe_consumed = false;
    let observed = allocations(|| {
        for iteration in 0..32 {
            assert_eq!(
                dispatcher.dispatch(
                    &Request::SendKey {
                        session,
                        key: probe_key,
                    },
                    &mut out,
                ),
                Reply::Output
            );
            if iteration != 31 {
                out.clear();
            }
        }
        out.clear();
        for iteration in 0..32 {
            assert_eq!(
                dispatcher.dispatch(
                    &Request::ProbeKey {
                        session,
                        scope: InputScope::Normal,
                        fresh_context: false,
                        key: probe_key,
                    },
                    &mut out,
                ),
                Reply::Output
            );
            if iteration != 31 {
                out.clear();
            }
        }
        legacy_probe_consumed = out.consumed;
        out.clear();
        for iteration in 0..32 {
            assert_eq!(
                dispatcher.dispatch(
                    &Request::ProbeKey {
                        session,
                        scope: InputScope::Normal,
                        fresh_context: true,
                        key: probe_key,
                    },
                    &mut out,
                ),
                Reply::Output
            );
            if iteration != 31 {
                out.clear();
            }
        }
        fresh_probe_consumed = out.consumed;
    });

    assert_eq!(observed, 0, "test_only Probe/ProbeKey dispatch allocated");
    assert_eq!(
        fresh_probe_consumed, legacy_probe_consumed,
        "scope-carrying fresh Probe must preserve legacy Probe consumption"
    );
    let probe_consumed = fresh_probe_consumed;
    assert_eq!(
        dispatcher.dispatch(
            &Request::SetInputScope {
                session,
                scope: InputScope::Normal,
            },
            &mut out,
        ),
        Reply::Message(Response::Ok)
    );
    let real_key = KeyInput {
        test_only: false,
        ..probe_key
    };
    assert_eq!(
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: real_key,
            },
            &mut out,
        ),
        Reply::Output
    );
    assert_eq!(
        out.consumed, probe_consumed,
        "Probe and Apply must preserve key-consumption parity"
    );
}

fn conversion_service() -> Arc<ConversionService> {
    let entries = parse_entries(
        "zero-alloc.tsv",
        "# license: MIT\n\
         reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
         かんすう\t函数\t1\t1\t1000\t-\t\t\n\
         かんすう\t関数\t1\t1\t1100\t600\tit,predict\tprogramming\n\
         もれ\t漏れ\t1\t1\t0\t-\t\t\n\
         ないか\t内科\t1\t1\t50\t-\t\t\n\
         ないか\tないか\t1\t1\t100\t-\t\t\n\
         ないか\t無いか\t1\t1\t110\t-\t\t\n\
         もれないか\t漏れないか\t1\t1\t10\t-\t\t\n",
    )
    .expect("entries");
    let matrix = parse_connection(
        "zero-alloc-matrix.tsv",
        "# license: MIT\nclasses\t3\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    let image = compile(&entries, &matrix)
        .expect("image")
        .into_boxed_slice();
    let bytes = Box::leak(image);
    Arc::new(ConversionService::from_static_bytes(bytes).expect("conversion service"))
}

/// The conversion hand-off is part of the steady-state keystroke contract,
/// not a permission to allocate because it happens less often than typing.
#[test]
fn conversion_into_reused_candidate_buffers_allocates_nothing() {
    let mut dispatcher =
        Dispatcher::new_with_conversion(conversion_service()).expect("shipped defaults");
    let mut out = OutputBuf::new();
    let session = match dispatcher.dispatch(
        &Request::CreateSession {
            process_name: "conversion-alloc.exe".to_owned(),
        },
        &mut out,
    ) {
        Reply::Message(Response::SessionCreated { session, .. }) => session,
        other => panic!("expected SessionCreated, got {other:?}"),
    };
    for character in "kannsuu".chars() {
        assert_eq!(
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: char_key(character),
                },
                &mut out,
            ),
            Reply::Output
        );
    }
    out.clear();

    let observed = allocations(|| {
        let reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: KeyInput {
                    code: KeyCode::Space,
                    ch: None,
                    modifiers: Modifiers::NONE,
                    repeat: false,
                    test_only: false,
                },
            },
            &mut out,
        );
        assert_eq!(reply, Reply::Output);
    });

    assert_eq!(observed, 0, "conversion dispatch allocated");
    assert!(out.has_candidates());
    assert_eq!(out.candidate(0).map(|candidate| candidate.0), Some("関数"));
}

#[test]
fn cross_commit_bridge_conversion_allocates_nothing() {
    // The ordinary input-repair subsystem owns separate per-query allocations
    // today. Disable it here so this test isolates the optional bridge and
    // proves that its second lattice pass, scratch copy, and rescore add none.
    let conversion = conversion_service();
    let mut options = ConversionOptions::default();
    options.input_support.enabled = false;
    let (prefix_right_id, prefix_cost) = conversion
        .with_conversion_input_bridge_hints(
            ConversionInput::ordinary("もれ"),
            options,
            &[],
            None,
            |candidates, _| {
                let tail = candidates
                    .iter()
                    .find(|candidate| candidate.text() == "漏れ")
                    .expect("lexical tail");
                (
                    RightContextId::new(tail.segments().last().expect("tail segment").right_id),
                    conversion
                        .cross_commit_prefix_cost(tail)
                        .expect("system-only tail prefix cost"),
                )
            },
        )
        .expect("tail conversion");
    let bridge = CrossCommitBridge {
        tail_reading: "もれ",
        tail_surface: "漏れ",
        prefix_right_id,
        prefix_cost,
    };

    // Warm both the direct and combined paths in the same converter slot.
    conversion
        .with_conversion_input_bridge_hints(
            ConversionInput::ordinary("ないか"),
            options,
            &[],
            Some(bridge),
            |_, _| (),
        )
        .expect("bridge warm-up");

    let mut plain_is_first = false;
    let mut negative_before_clinic = false;
    let observed = allocations(|| {
        conversion
            .with_conversion_input_bridge_hints(
                ConversionInput::ordinary("ないか"),
                options,
                &[],
                Some(bridge),
                |candidates, _| {
                    plain_is_first = candidates
                        .first()
                        .is_some_and(|candidate| candidate.text() == "ないか");
                    let negative = candidates
                        .iter()
                        .position(|candidate| candidate.text() == "無いか");
                    let clinic = candidates
                        .iter()
                        .position(|candidate| candidate.text() == "内科");
                    negative_before_clinic = negative
                        .zip(clinic)
                        .is_some_and(|(negative, clinic)| negative < clinic);
                },
            )
            .expect("measured bridge conversion");
    });

    assert_eq!(observed, 0, "cross-commit conversion allocated");
    assert!(plain_is_first);
    assert!(negative_before_clinic);
}

/// Prediction crosses the single-slot mailbox and copies the worker result
/// into the dispatcher's preallocated cache without allocating on the pipe
/// thread that owns the keystroke.
#[test]
fn prediction_handoff_into_reused_buffers_allocates_nothing() {
    let conversion = conversion_service();
    let learning = Arc::new(LearningService::memory());
    let runtime = PredictionRuntime::start(Arc::clone(&conversion)).expect("prediction runtime");
    let mut dispatcher = Dispatcher::new_with_runtime_configuration(
        conversion,
        learning,
        runtime.service(),
        Preferences::default(),
    )
    .expect("shipped defaults");
    let mut out = OutputBuf::new();
    let session = match dispatcher.dispatch(
        &Request::CreateSession {
            process_name: "prediction-alloc.exe".to_owned(),
        },
        &mut out,
    ) {
        Reply::Message(Response::SessionCreated { session, .. }) => session,
        other => panic!("expected SessionCreated, got {other:?}"),
    };
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('k'),
        },
        &mut out,
    );
    out.clear();

    // The engine waits at most its 10 ms prediction window per keystroke and
    // fails open with no candidates — deliberately, so a keystroke is never
    // blocked on the worker. On a loaded test host the worker can miss that
    // window, so the test retypes the keystroke until the handoff really
    // happens instead of betting everything on one window. What it asserts
    // unconditionally is the property under test: no attempt — delivering
    // or not — allocates on the dispatch thread.
    let mut delivered = false;
    for attempt in 0..50 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            // Backspace removes the whole resolved か, so retype the k to
            // put the composition back where the measured keystroke expects.
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: KeyInput {
                        code: KeyCode::Backspace,
                        ch: None,
                        modifiers: Modifiers::NONE,
                        repeat: false,
                        test_only: false,
                    },
                },
                &mut out,
            );
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: char_key('k'),
                },
                &mut out,
            );
            out.clear();
        }
        let observed = allocations(|| {
            let reply = dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: char_key('a'),
                },
                &mut out,
            );
            assert_eq!(reply, Reply::Output);
        });
        assert_eq!(observed, 0, "prediction dispatch allocated");
        if out.has_candidates() {
            delivered = true;
            break;
        }
        out.clear();
    }

    assert!(
        delivered,
        "the prediction worker never delivered candidates"
    );
    assert_eq!(out.candidate(0).map(|candidate| candidate.0), Some("関数"));
    runtime.stop().expect("prediction worker joins");
}
