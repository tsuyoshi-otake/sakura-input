//! The hot path allocates nothing (DESIGN 5.7), asserted rather than claimed.
//!
//! "Zero allocations per keystroke" is a promise that decays silently: a
//! `String` slipped into a signature, a `to_string()` in an error path, a
//! `Vec` built to hold something that could have been written straight into
//! the sink — none of it changes a test result, and all of it changes what
//! the IME costs on every key the user presses. So this counts.
//!
//! Everything expensive happens once, at load: compiling the romaji table and
//! the key map allocates, and is supposed to. What must not allocate is what
//! runs afterwards, per keystroke — feeding the FSM, normalizing width,
//! resolving a key binding.
//!
//! The counter is per-thread, so the test harness printing results on another
//! thread cannot make this pass or fail by accident.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use sakura_core::keymap::{KeyMap, Preset, State};
use sakura_core::romaji::{Input, Table};
use sakura_core::width::{BracketStyle, Normalizer, PunctuationStyle, Width, WidthPolicy};
use sakura_proto::{FixedStr, KeyCode, KeyInput, Mode, Modifiers};

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

/// The kana path, which runs on literally every keystroke.
#[test]
fn feeding_the_romaji_fsm_allocates_nothing() {
    let table = Table::builtin().expect("the shipped table must compile");
    let mut state = Input::new();
    // Sized for a preedit, allocated once, reused for the whole session.
    let mut out = FixedStr::<512>::new();

    // Warm up outside the measurement: the first pass through any lazily
    // initialized machinery is not what a keystroke costs.
    for c in "sakura".chars() {
        table.feed(&mut state, c, &mut out).expect("fits");
    }
    table.flush(&mut state, &mut out).expect("fits");
    out.clear();

    let observed = allocations(|| {
        // Every shape the FSM has: plain syllables, a waiting `n`, a
        // backtrack, a carry, a passthrough, and a flush.
        for c in "konnnichihakekkadockern".chars() {
            table.feed(&mut state, c, &mut out).expect("fits");
        }
        table.flush(&mut state, &mut out).expect("fits");
        state.backspace();
    });

    assert_eq!(observed, 0, "the kana path allocated");
    assert_eq!(out.as_str(), "こんにちはけっかどcけrん");
}

/// The width choke point, which every string leaving the engine passes
/// through (DESIGN 5.6).
#[test]
fn normalizing_width_allocates_nothing() {
    let normalizer = Normalizer {
        width: WidthPolicy {
            alnum: Width::Full,
            number: Width::FollowMode,
            symbol: Width::Half,
        },
        punctuation: PunctuationStyle::CommaPeriod,
        brackets: BracketStyle::default(),
    };
    let mut out = FixedStr::<512>::new();

    normalizer
        .normalize_into("warmup", Mode::Hiragana, &mut out)
        .expect("fits");
    out.clear();

    let observed = allocations(|| {
        for mode in Mode::ALL {
            out.clear();
            normalizer
                .normalize_into("docker 123、abc。", mode, &mut out)
                .expect("fits");
        }
    });

    assert_eq!(observed, 0, "the width choke point allocated");
}

/// Key resolution, which runs before anything else on every keystroke.
#[test]
fn looking_up_a_key_binding_allocates_nothing() {
    let map = KeyMap::preset(Preset::MsIme).expect("the shipped preset must compile");
    let events = [
        KeyInput {
            code: KeyCode::Space,
            ch: None,
            modifiers: Modifiers::NONE,
            repeat: false,
            test_only: false,
        },
        KeyInput {
            code: KeyCode::Enter,
            ch: None,
            modifiers: Modifiers::SHIFT,
            repeat: false,
            test_only: false,
        },
        KeyInput {
            code: KeyCode::Char,
            ch: Some('U'),
            modifiers: Modifiers::CTRL,
            repeat: false,
            test_only: false,
        },
        // An unbound key, so the global fallback runs too.
        KeyInput {
            code: KeyCode::Char,
            ch: Some('a'),
            modifiers: Modifiers::NONE,
            repeat: false,
            test_only: false,
        },
    ];

    for state in State::ALL {
        for event in &events {
            let _ = map.lookup(state, event);
        }
    }

    let observed = allocations(|| {
        for state in State::ALL {
            for event in &events {
                let _ = map.lookup(state, event);
            }
        }
    });

    assert_eq!(observed, 0, "key lookup allocated");
}

/// The three together, in the order a keystroke actually visits them: the
/// key map decides the key is text, the FSM turns it into kana, and the
/// choke point normalizes what comes out.
#[test]
fn a_whole_keystroke_allocates_nothing() {
    let map = KeyMap::preset(Preset::MsIme).expect("the shipped preset must compile");
    let table = Table::builtin().expect("the shipped table must compile");
    let normalizer = Normalizer::default();

    let mut state = Input::new();
    let mut kana = FixedStr::<512>::new();
    let mut out = FixedStr::<512>::new();

    let keystroke = |state: &mut Input, kana: &mut FixedStr<512>, out: &mut FixedStr<512>, c| {
        let event = KeyInput {
            code: KeyCode::Char,
            ch: Some(c),
            modifiers: Modifiers::NONE,
            repeat: false,
            test_only: false,
        };
        if map.lookup(State::Composing, &event).is_none() {
            kana.clear();
            table.feed(state, c, kana).expect("fits");
            normalizer
                .normalize_into(kana.as_str(), Mode::Hiragana, out)
                .expect("fits");
        }
    };

    for c in "warmup".chars() {
        keystroke(&mut state, &mut kana, &mut out, c);
    }
    state.clear();
    out.clear();

    let observed = allocations(|| {
        for c in "sakurainput".chars() {
            keystroke(&mut state, &mut kana, &mut out, c);
        }
    });

    assert_eq!(observed, 0, "the keystroke path allocated");
    assert_eq!(out.as_str(), "さくらいんぷ");
}
