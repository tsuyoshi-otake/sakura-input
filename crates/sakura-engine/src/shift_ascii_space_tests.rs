use crate::shift_ascii_space::{
    decide_shift_ascii_convert, ShiftAsciiConvertDecision, ShiftAsciiConvertFacts,
};

/// Independent declarative oracle. The rows describe user-visible semantics;
/// they do not reproduce the production branch order.
fn oracle(facts: ShiftAsciiConvertFacts) -> ShiftAsciiConvertDecision {
    match (
        facts.shifted_ascii,
        facts.trigger_is_space,
        facts.shift_modifier,
        facts.dictionary_hit,
    ) {
        (false, _, _, _) => ShiftAsciiConvertDecision::Convert,
        (true, true, true, _) | (true, true, false, false) => {
            ShiftAsciiConvertDecision::InsertLiteralSpace
        }
        (true, _, _, true) => ShiftAsciiConvertDecision::Convert,
        (true, _, _, false) => ShiftAsciiConvertDecision::RejectUnknown,
    }
}

#[test]
fn concrete_decision_examples_match_the_independent_oracle() {
    let examples = [
        (
            "Japanese composition keeps conversion",
            ShiftAsciiConvertFacts {
                shifted_ascii: false,
                trigger_is_space: true,
                shift_modifier: false,
                dictionary_hit: false,
            },
        ),
        (
            "known English term converts on plain Space",
            ShiftAsciiConvertFacts {
                shifted_ascii: true,
                trigger_is_space: true,
                shift_modifier: false,
                dictionary_hit: true,
            },
        ),
        (
            "unknown English text gets a plain literal Space",
            ShiftAsciiConvertFacts {
                shifted_ascii: true,
                trigger_is_space: true,
                shift_modifier: false,
                dictionary_hit: false,
            },
        ),
        (
            "Shift+Space is explicit even for a known term",
            ShiftAsciiConvertFacts {
                shifted_ascii: true,
                trigger_is_space: true,
                shift_modifier: true,
                dictionary_hit: true,
            },
        ),
        (
            "unknown non-Space conversion remains rejected",
            ShiftAsciiConvertFacts {
                shifted_ascii: true,
                trigger_is_space: false,
                shift_modifier: false,
                dictionary_hit: false,
            },
        ),
        (
            "known non-Space conversion remains available",
            ShiftAsciiConvertFacts {
                shifted_ascii: true,
                trigger_is_space: false,
                shift_modifier: true,
                dictionary_hit: true,
            },
        ),
    ];

    for (name, facts) in examples {
        assert_eq!(decide_shift_ascii_convert(facts), oracle(facts), "{name}");
    }
}

#[test]
fn shift_ascii_space_pbt_and_c2_cover_all_atomic_conditions() {
    const SEED: u64 = 0x5341_4b55_5241_0056;
    let mut random = SEED;
    let mut condition_seen = [[false; 2]; 4];
    let mut combinations = [false; 16];

    for _ in 0..4_096 {
        random ^= random >> 12;
        random ^= random << 25;
        random ^= random >> 27;
        let bits = random.wrapping_mul(0x2545_f491_4f6c_dd1d) as usize;
        let facts = ShiftAsciiConvertFacts {
            shifted_ascii: bits & 1 != 0,
            trigger_is_space: bits & 2 != 0,
            shift_modifier: bits & 4 != 0,
            dictionary_hit: bits & 8 != 0,
        };
        let values = [
            facts.shifted_ascii,
            facts.trigger_is_space,
            facts.shift_modifier,
            facts.dictionary_hit,
        ];
        let mut index = 0usize;
        for (condition, value) in values.into_iter().enumerate() {
            condition_seen[condition][usize::from(value)] = true;
            if value {
                index |= 1 << condition;
            }
        }
        combinations[index] = true;
        assert_eq!(decide_shift_ascii_convert(facts), oracle(facts));
    }

    // The random campaign is replayable, while this exhaustive tail makes the
    // quantified property and C2 denominator independent of RNG distribution.
    for index in 0..16usize {
        let facts = ShiftAsciiConvertFacts {
            shifted_ascii: index & 1 != 0,
            trigger_is_space: index & 2 != 0,
            shift_modifier: index & 4 != 0,
            dictionary_hit: index & 8 != 0,
        };
        combinations[index] = true;
        assert_eq!(decide_shift_ascii_convert(facts), oracle(facts));
    }

    let covered = condition_seen
        .into_iter()
        .flatten()
        .filter(|seen| *seen)
        .count();
    let total = 8usize;
    assert_eq!(covered, total);
    assert!(combinations.into_iter().all(|seen| seen));
    eprintln!(
        "shift-ascii-space PBT seed={SEED:#018x}; random_cases=4096; exhaustive_cases=16; C2={covered}/{total}=100%"
    );
}
