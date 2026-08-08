//! Deterministic Phase 5 campaign for the input and key-map state machines.
//!
//! The normal target is a short regression guard. The ignored target is the
//! resumable soak entry point and accepts a shard plus a per-slice seed. Every
//! operation is bounded: the romaji pending buffer and output sink have fixed
//! capacities, and each iteration performs exactly one event.

use std::panic::{catch_unwind, AssertUnwindSafe};

use sakura_core::keymap::{Action, KeyMap, Preset, State};
use sakura_core::romaji::{Input, Table, MAX_SEQUENCE};
use sakura_proto::{FixedStr, KeyCode, KeyInput, Modifiers};

const DEFAULT_ITERATIONS: u64 = 20_000;
const OUTPUT_CAPACITY: usize = 1_024;

#[test]
fn arbitrary_fsm_events_terminate_and_stay_bounded() {
    campaign(DEFAULT_ITERATIONS, 0, 0);
}

#[test]
#[ignore = "long deterministic campaign; set SAKURA_FUZZ_ITERS, SAKURA_FUZZ_SHARD, and SAKURA_FUZZ_SEED"]
fn sharded_fsm_campaign() {
    let iterations = env_u64("SAKURA_FUZZ_ITERS").unwrap_or(2_000_000);
    let shard = env_u64("SAKURA_FUZZ_SHARD").unwrap_or(0);
    let slice_seed = env_u64("SAKURA_FUZZ_SEED").unwrap_or(0);
    campaign(iterations, shard, slice_seed);
}

fn campaign(iterations: u64, shard: u64, slice_seed: u64) {
    let table = Table::builtin().expect("shipped romaji table");
    let key_map = KeyMap::preset(Preset::MsIme).expect("shipped key map");
    let mut input = Input::new();
    let mut output = FixedStr::<OUTPUT_CAPACITY>::new();
    let mut state = State::Idle;
    let mut random =
        Random::new(0xf511_72a9_5eed_0001 ^ shard.rotate_left(17) ^ slice_seed.rotate_left(31));

    for iteration in 0..iterations {
        let seed = random.state;
        let event = random.usize(8);
        let outcome = catch_unwind(AssertUnwindSafe(|| match event {
            0 => {
                let key = (b'a' + random.usize(26) as u8) as char;
                let _ = table.feed(&mut input, key, &mut output);
            }
            1 => {
                let key = (b'A' + random.usize(26) as u8) as char;
                let _ = table.feed(&mut input, key, &mut output);
            }
            2 => {
                const HOSTILE_CHARS: [char; 10] = [
                    '\0',
                    '\n',
                    '\u{7f}',
                    'あ',
                    '漢',
                    '🚀',
                    '\u{200d}',
                    '\u{feff}',
                    '\u{10ffff}',
                    'ß',
                ];
                let _ = table.feed(
                    &mut input,
                    HOSTILE_CHARS[random.usize(HOSTILE_CHARS.len())],
                    &mut output,
                );
            }
            3 => {
                let _ = input.backspace();
            }
            4 => input.clear(),
            5 => {
                let _ = table.flush(&mut input, &mut output);
            }
            6 | 7 => {
                let code = KeyCode::ALL[random.usize(KeyCode::ALL.len())];
                let key = KeyInput {
                    code,
                    ch: (code == KeyCode::Char).then(|| {
                        const PRINTABLE: [char; 8] = ['a', 'Z', '0', ' ', 'あ', '漢', '🚀', '\0'];
                        PRINTABLE[random.usize(PRINTABLE.len())]
                    }),
                    modifiers: Modifiers(random.next() as u8),
                    repeat: random.usize(2) == 0,
                    test_only: random.usize(2) == 0,
                };
                if let Some(action) = key_map.lookup(state, &key) {
                    state = transition(state, action);
                } else if state == State::Idle && code == KeyCode::Char {
                    state = State::Composing;
                }
            }
            _ => unreachable!("event is reduced modulo eight"),
        }));

        assert!(
            outcome.is_ok(),
            "FSM panicked at shard {shard}, slice seed {slice_seed}, iteration {iteration}, PRNG seed {seed:#018x}, event {event}"
        );
        assert!(
            input.pending().len() <= MAX_SEQUENCE,
            "pending buffer escaped its bound at shard {shard}, iteration {iteration}"
        );
        assert!(
            input.pending().is_ascii(),
            "pending romaji ceased to be ASCII at shard {shard}, iteration {iteration}"
        );
        assert!(output.len() <= OUTPUT_CAPACITY);

        // A full sink is an expected, explicit terminal result. Clearing it
        // models the engine handing the completed output to its caller before
        // accepting the next event; no retry loop is hidden in the campaign.
        if output.len() > OUTPUT_CAPACITY - 64 {
            output.clear();
        }
    }

    let _ = table.flush(&mut input, &mut output);
    assert!(input.pending().len() <= MAX_SEQUENCE);
}

fn transition(current: State, action: Action) -> State {
    match action {
        Action::ImeOff | Action::ModeDirect | Action::Commit | Action::CommitFirst => State::Idle,
        Action::Convert
        | Action::ConvertPrev
        | Action::CandidateNext
        | Action::CandidatePrev
        | Action::CandidatePageDown
        | Action::CandidatePageUp
        | Action::CandidateExpand => State::Converting,
        Action::PredictNext | Action::PredictPrev => State::Predicting,
        Action::Cancel => match current {
            State::Converting | State::Predicting => State::Composing,
            State::Composing | State::Idle => State::Idle,
        },
        _ if current == State::Idle => State::Composing,
        _ => current,
    }
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse().ok()
}

struct Random {
    state: u64,
}

impl Random {
    const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        let mut value = self.state;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        self.state = value;
        value.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn usize(&mut self, exclusive_end: usize) -> usize {
        if exclusive_end == 0 {
            0
        } else {
            (self.next() as usize) % exclusive_end
        }
    }
}
