use super::*;

fn builtin() -> Table {
    Table::builtin().expect("the shipped table must compile")
}

/// Types `input` and leaves whatever is pending pending, the way the IME
/// behaves mid-word.
fn typed(table: &Table, input: &str) -> (String, String) {
    let mut state = Input::new();
    let mut out = String::new();
    for c in input.chars() {
        table.feed(&mut state, c, &mut out).expect("String sink");
    }
    (out, state.pending().to_string())
}

/// Types `input` and commits, the way Enter behaves.
fn committed(table: &Table, input: &str) -> String {
    let mut state = Input::new();
    let mut out = String::new();
    for c in input.chars() {
        table.feed(&mut state, c, &mut out).expect("String sink");
    }
    table.flush(&mut state, &mut out).expect("String sink");
    assert!(state.is_empty(), "flush must leave nothing pending");
    out
}

#[test]
fn replay_local_completion_finds_only_table_derived_positive_controls() {
    let table = builtin();

    let nazka = table.replay("nazka").expect("bounded replay");
    assert_eq!(nazka.output(), "なzか");
    assert_eq!(nazka.raw_passthrough_count(), 1);
    assert_eq!(nazka.events()[0].raw_start(), 0);
    assert_eq!(nazka.events()[0].raw_end(), 2);
    assert_eq!(nazka.events()[1].kind(), ReplayEventKind::RawPassthrough);
    assert_eq!(nazka.events()[1].raw_start(), 2);
    assert_eq!(nazka.events()[1].raw_end(), 3);
    assert_eq!(nazka.events()[2].raw_start(), 3);
    assert_eq!(nazka.events()[2].raw_end(), 5);

    let plans = table
        .plan_local_completions("nazka", "なzか")
        .expect("matching observed preedit");
    assert_eq!(plans.len(), 5);
    let nazka_target = plans
        .iter()
        .find(|plan| plan.key == b'e')
        .expect("e completion");
    assert_eq!(nazka_target.corrected_reading.as_str(), "なぜか");
    assert_eq!(nazka_target.insertion_at, 3);
    assert_eq!(
        (nazka_target.anomaly_start, nazka_target.anomaly_end),
        (2, 3)
    );

    let naikniiku = table.replay("naikniiku").expect("bounded replay");
    assert_eq!(naikniiku.output(), "ないkにいく");
    let plans = table
        .plan_local_completions("naikniiku", "ないkにいく")
        .expect("matching observed preedit");
    assert_eq!(plans.len(), 5);
    let naikniiku_target = plans
        .iter()
        .find(|plan| plan.key == b'a')
        .expect("a completion");
    assert_eq!(naikniiku_target.corrected_reading.as_str(), "ないかにいく");
    assert_eq!(naikniiku_target.insertion_at, 4);
    assert_eq!(
        (naikniiku_target.anomaly_start, naikniiku_target.anomaly_end),
        (3, 4)
    );
}

#[test]
fn replay_local_completion_rejects_normal_or_nonlocal_controls() {
    let table = builtin();

    for (raw, observed, corrected) in [
        ("nazeka", "なぜか", "なぜか"),
        ("naikaniiku", "ないかにいく", "ないかにいく"),
        ("naeka", "なえか", "なぜか"),
        ("nazea", "なぜあ", "なぜあ"),
        ("nazq", "なzq", "なぜか"),
    ] {
        let plans = table
            .plan_local_completions(raw, observed)
            .expect("observed output must match replay");
        assert!(
            plans.is_empty(),
            "unexpected repair for {raw:?} toward {corrected:?}"
        );
    }

    assert_eq!(
        table.plan_local_completions("なぜか", "なぜか"),
        Err(LocalCompletionError::Replay(ReplayError::NonAsciiRaw))
    );
    assert_eq!(
        table.plan_local_completions("nazka", "なぜか"),
        Err(LocalCompletionError::ObservedMismatch)
    );
}

#[test]
fn replay_preserves_n_prefixes_and_carry_source_overlap() {
    let table = builtin();

    let n = table.replay("n").expect("bounded replay");
    assert_eq!(n.output(), "");
    assert_eq!(n.pending(), "n");
    assert_eq!(n.raw_passthrough_count(), 0);
    assert_eq!(table.replay_committed("n").unwrap().output(), "ん");

    let nn = table.replay("nn").expect("bounded replay");
    assert_eq!(nn.output(), "ん");
    assert!(nn.pending().is_empty());
    assert_eq!(table.replay_committed("nn").unwrap().output(), "ん");
    assert_eq!(table.replay_committed("n'").unwrap().output(), "ん");

    let carry = table.replay("ttu").expect("bounded replay");
    assert_eq!(carry.output(), "っつ");
    assert!(carry.pending().is_empty());
    assert_eq!(carry.carry_overlap(), 0);
    assert_eq!(carry.events().len(), 2);
    assert_eq!(
        (
            carry.events()[0].raw_start(),
            carry.events()[0].raw_end(),
            carry.events()[0].kind()
        ),
        (0, 2, ReplayEventKind::Kana)
    );
    assert_eq!(
        (carry.events()[1].raw_start(), carry.events()[1].raw_end()),
        (1, 3)
    );
}

#[test]
fn replay_uses_custom_table_and_is_deterministic() {
    let table = Table::parse(
        "[kana]\n\
             qa = \"α\"\n\
             x = \"え\"\n",
    )
    .expect("custom table");
    let trace = table.replay("qx").expect("bounded replay");
    assert_eq!(trace.output(), "qえ");
    assert_eq!(trace.raw_passthrough_count(), 1);
    let plans = table
        .plan_local_completions("qx", "qえ")
        .expect("matching custom replay");
    assert_eq!(plans.len(), 1);
    assert_eq!(plans.get(0).unwrap().key, b'a');
    assert_eq!(plans.get(0).unwrap().insertion_at, 1);
    assert_eq!(plans.get(0).unwrap().corrected_reading.as_str(), "αえ");

    assert_eq!(table.replay("qx").unwrap(), table.replay("qx").unwrap());
    assert_eq!(
        table.replay("t").unwrap().output(),
        table.replay("t").unwrap().output()
    );
    let too_long = "a".repeat(MAX_REPLAY_RAW_BYTES + 1);
    assert_eq!(table.replay(&too_long), Err(ReplayError::RawTooLong));
}

#[test]
fn the_shipped_table_compiles() {
    let table = builtin();
    assert!(table.len() > 200, "unexpectedly small: {}", table.len());
    assert!(!table.is_empty());
}

/// Every entry must be reachable by typing its own sequence. An entry that
/// is shadowed by the matching rules is dead weight the author cannot see.
#[test]
fn every_carry_free_entry_is_reachable_by_typing_it() {
    let table = builtin();
    for entry in &table.entries {
        if !entry.carry.is_empty() {
            continue;
        }
        assert_eq!(
            committed(&table, &entry.sequence),
            entry.output,
            "entry {:?} is unreachable",
            entry.sequence
        );
    }
}

/// The carrying entries, checked the same way: typing the sequence emits
/// the output and leaves the carry to resolve.
#[test]
fn every_carrying_entry_emits_its_output_and_carries_on() {
    let table = builtin();
    for entry in &table.entries {
        if entry.carry.is_empty() {
            continue;
        }
        let (emitted, _) = typed(&table, &entry.sequence);
        assert_eq!(
            emitted, entry.output,
            "entry {:?} did not emit its output",
            entry.sequence
        );
    }
}

#[test]
fn vowels_and_basic_syllables() {
    let table = builtin();
    assert_eq!(committed(&table, "aiueo"), "あいうえお");
    assert_eq!(committed(&table, "kakikukeko"), "かきくけこ");
    assert_eq!(committed(&table, "sakura"), "さくら");
    assert_eq!(committed(&table, "nihongo"), "にほんご");
}

#[test]
fn contracted_sounds() {
    let table = builtin();
    assert_eq!(committed(&table, "kyou"), "きょう");
    assert_eq!(committed(&table, "shain"), "しゃいん");
    assert_eq!(committed(&table, "chotto"), "ちょっと");
    assert_eq!(committed(&table, "jugyou"), "じゅぎょう");
}

/// The `n` cases, which are the reason the matcher backtracks at all.
#[test]
fn n_resolves_by_what_follows_it() {
    let table = builtin();
    // Followed by a consonant: ん, and the consonant starts a new sequence.
    assert_eq!(committed(&table, "genki"), "げんき");
    assert_eq!(committed(&table, "shinkansen"), "しんかんせん");
    // Followed by a vowel: the longer reading wins.
    assert_eq!(committed(&table, "kani"), "かに");
    assert_eq!(committed(&table, "sunao"), "すなお");
    // An apostrophe provides a concise ん boundary before a な-row or
    // や-row kana. Without it `honya` reads ほにゃ. With the
    // Microsoft-compatible `nn` rule, `honnya` is ほんや; a third `n`
    // makes the next syllable にゃ.
    assert_eq!(committed(&table, "hon'ya"), "ほんや");
    assert_eq!(committed(&table, "honya"), "ほにゃ");
    assert_eq!(committed(&table, "honnya"), "ほんや");
    assert_eq!(committed(&table, "honnnya"), "ほんにゃ");
    assert_eq!(committed(&table, "konnyaku"), "こんやく");
    assert_eq!(committed(&table, "konnnyaku"), "こんにゃく");
    // Alone at the end of input.
    assert_eq!(committed(&table, "n"), "ん");
    assert_eq!(committed(&table, "pan"), "ぱん");
}

/// Microsoft IME treats `nn` as an explicit ん, even before a vowel.
/// A following な-row syllable therefore needs a third `n`.
#[test]
fn microsoft_double_n_commits_n_before_a_vowel() {
    let table = builtin();
    assert_eq!(committed(&table, "hannei"), "はんえい");
    assert_eq!(committed(&table, "minna"), "みんあ");
    assert_eq!(committed(&table, "annai"), "あんあい");
    assert_eq!(committed(&table, "onnanoko"), "おんあのこ");
    // The third `n` begins the following な-row syllable.
    assert_eq!(committed(&table, "minnna"), "みんな");
    assert_eq!(committed(&table, "annnai"), "あんない");
    assert_eq!(committed(&table, "konnnichiha"), "こんにちは");
}

/// Mid-word, a lone `n` stays pending so the user can still reach `な`.
#[test]
fn a_lone_n_waits_before_committing_to_a_reading() {
    let table = builtin();
    let (emitted, pending) = typed(&table, "n");
    assert_eq!(emitted, "");
    assert_eq!(pending, "n");
}

#[test]
fn sokuon_comes_from_the_doubled_consonant() {
    let table = builtin();
    assert_eq!(committed(&table, "kekka"), "けっか");
    assert_eq!(committed(&table, "matte"), "まって");
    assert_eq!(committed(&table, "kitto"), "きっと");
    assert_eq!(committed(&table, "asatte"), "あさって");
    assert_eq!(committed(&table, "zasshi"), "ざっし");
    assert_eq!(committed(&table, "happa"), "はっぱ");
    // Explicit small tsu, both spellings.
    assert_eq!(committed(&table, "xtu"), "っ");
    assert_eq!(committed(&table, "ltsu"), "っ");
}

/// `nn` is ん, not a sokuon. Every other doubled consonant is a sokuon.
#[test]
fn doubled_n_is_not_a_sokuon() {
    let table = builtin();
    assert_eq!(committed(&table, "nn"), "ん");
    assert_eq!(committed(&table, "annnai"), "あんない");
}

/// EVAL: scans every shipped entry for the exact structural shape that
/// causes a live-typing stall -- an entry that is itself a complete,
/// valid mapping (`Table::drive` could commit it right now) but is also
/// a strict prefix of one or more longer entries, so `may_wait` in
/// `Table::drive` holds it pending until another key (or an explicit
/// flush) arrives. Only `n` has that shape in the Microsoft-compatible
/// shipped table. `nn` must commit ん immediately so `hannei` cannot
/// become はんねい. If a future table edit (including a user's custom
/// table) adds another entry with this shape, this fails so the same
/// stall gets a deliberate look instead of shipping silently.
#[test]
fn only_n_is_a_complete_entry_that_still_waits_for_more() {
    let table = builtin();
    let mut stalls: Vec<&str> = table
        .entries
        .iter()
        .filter(|entry| entry.carry.is_empty())
        .filter(|entry| table.extends(&entry.sequence))
        .map(|entry| entry.sequence.as_str())
        .collect();
    stalls.sort_unstable();
    assert_eq!(
        stalls,
        vec!["n"],
        "an entry both commits on its own and waits for more input; \
             review whether the live-typing stall this causes (raw romaji \
             stays on screen until another key or a flush) is intended"
    );
}

/// EVAL: `data/romaji.toml` spells every small kana two ways -- `x` and
/// `l` prefixes (`xa`/`la`, `xtu`/`ltu`, `xka`/`lka`, ...) -- so both the
/// long-standing `x` convention and the newer `l` one work identically.
/// Nothing but this test enforces that they stay identical: the compiler
/// has no notion that `xa` and `la` are supposed to agree, so a future
/// table edit that changes one prefix's output but not the other's would
/// compile cleanly and silently make the two spellings of "the same
/// small kana" produce different kana. `xn` is excluded on purpose -- it
/// is an alternate spelling of ん (see `xn = "ん"` in the table), not a
/// small-kana prefix, and has no `l` counterpart to compare against.
#[test]
fn x_and_l_small_kana_prefixes_stay_in_sync() {
    let table = builtin();
    let x_forms: std::collections::BTreeMap<&str, &str> = table
        .entries
        .iter()
        .filter(|entry| entry.sequence != "xn")
        .filter_map(|entry| {
            entry
                .sequence
                .strip_prefix('x')
                .map(|rest| (rest, entry.output.as_str()))
        })
        .collect();
    let l_forms: std::collections::BTreeMap<&str, &str> = table
        .entries
        .iter()
        .filter_map(|entry| {
            entry
                .sequence
                .strip_prefix('l')
                .map(|rest| (rest, entry.output.as_str()))
        })
        .collect();
    assert_eq!(
        x_forms, l_forms,
        "every `x`-prefixed small-kana spelling must have an identical \
             `l`-prefixed twin, and vice versa"
    );
}

/// EVAL: `nn` commits ん immediately and every remaining `n` begins a new
/// decision. This is what makes `hannei` unambiguous, while spelling
/// ん+な as `minnna` remains available without an apostrophe.
#[test]
fn n_runs_follow_the_microsoft_double_n_rule() {
    let table = builtin();
    assert_eq!(
        committed(&table, "minna"),
        "みんあ",
        "two `n`s commit ん before the vowel"
    );
    assert_eq!(
        committed(&table, "minnna"),
        "みんな",
        "the third `n` starts な"
    );
    assert_eq!(committed(&table, "nn"), "ん");
    assert_eq!(
        committed(&table, "nnn"),
        "んん",
        "the remaining `n` becomes its own ん on commit"
    );
    assert_eq!(committed(&table, "denn"), "でん");
    assert_eq!(
        committed(&table, "dennn"),
        "でんん",
        "the third `n` is a separate trailing ん"
    );
}

/// EVAL: `every_carrying_entry_emits_its_output_and_carries_on` proves the
/// output half of a carrying entry via `feed` (mid-typing, where
/// `may_wait` is `true`); this proves the other half via `flush`
/// (`may_wait = false`), the code path `Enter` actually uses. `kk` alone
/// is unremarkable -- っ commits and the carried `k` passes through raw
/// because no bare `k` entry exists -- but nothing in the type system
/// forces that: a future table edit adding a bare single-consonant entry
/// (as `n` already is) would silently change what every sokuon carrying
/// that consonant commits to when nothing follows it. This locks today's
/// correct answer in so that change gets a deliberate look instead of
/// shipping as a side effect of an unrelated edit.
#[test]
fn every_carrying_entry_resolves_deterministically_when_flushed_alone() {
    let table = builtin();
    for entry in &table.entries {
        if entry.carry.is_empty() {
            continue;
        }
        let expected = format!("{}{}", entry.output, entry.carry);
        assert_eq!(
            committed(&table, &entry.sequence),
            expected,
            "entry {:?} did not resolve to output+carry when flushed alone",
            entry.sequence
        );
    }
}

#[test]
fn punctuation_and_the_long_vowel_mark() {
    let table = builtin();
    assert_eq!(committed(&table, "ra-men"), "らーめん");
    assert_eq!(committed(&table, "a,bi."), "あ、び。");
    assert_eq!(committed(&table, "[a]"), "「あ」");
    assert_eq!(committed(&table, "a/i"), "あ・い");
}

/// EVAL: `n'` provides a concise delimiter for ん before a な/や-row
/// syllable. Without it, the Microsoft-compatible table needs a third
/// `n` (`honnnya`) to produce ほんにゃ. Nothing extends past `n'` (see
/// `only_n_is_a_complete_entry_that_still_waits_for_more`), so it commits
/// ん the instant it is typed. This also verifies that an apostrophe stays
/// inert everywhere it does not follow an `n`.
#[test]
fn apostrophe_disambiguates_n_and_is_inert_elsewhere() {
    let table = builtin();
    assert_eq!(committed(&table, "n'"), "ん");
    let (emitted, pending) = typed(&table, "n'");
    assert_eq!(emitted, "ん", "`n'` should commit without waiting for more");
    assert_eq!(pending, "");
    // A second word pair alongside hon'ya/honya/honnnya, so the earlier
    // result is not an artifact of that one word's shape.
    assert_eq!(committed(&table, "kon'yaku"), "こんやく");
    assert_eq!(committed(&table, "konyaku"), "こにゃく");
    assert_eq!(committed(&table, "konnnyaku"), "こんにゃく");
    // With no preceding `n` to disambiguate, the apostrophe means
    // nothing and passes through like any other unmapped character --
    // it must not silently vanish.
    assert_eq!(committed(&table, "'"), "'");
    assert_eq!(committed(&table, "a'i"), "あ'い");
}

/// Letters with no reading pass through rather than disappearing — this is
/// what makes a mistyped word recoverable instead of silently eaten.
#[test]
fn unmapped_letters_pass_through() {
    let table = builtin();
    assert_eq!(committed(&table, "docker"), "どcけr");
    assert_eq!(committed(&table, "kit"), "きt");
    assert_eq!(committed(&table, "q"), "q");
    assert_eq!(committed(&table, "!?"), "!?");
}

#[test]
fn lookup_folds_ascii_case() {
    let table = builtin();
    assert_eq!(committed(&table, "KA"), "か");
    assert_eq!(committed(&table, "KoNnNiChiHa"), "こんにちは");
}

/// A non-ASCII character cannot be part of a sequence, so pending romaji
/// resolves first and the character follows it in typing order.
#[test]
fn non_ascii_input_flushes_pending_then_passes_through() {
    let table = builtin();
    assert_eq!(committed(&table, "kan字"), "かん字");
    assert_eq!(committed(&table, "a🍣"), "あ🍣");
}

#[test]
fn backspace_eats_pending_romaji_before_kana() {
    let table = builtin();
    let mut state = Input::new();
    let mut out = String::new();
    for c in "kaky".chars() {
        table.feed(&mut state, c, &mut out).expect("String sink");
    }
    assert_eq!(out, "か");
    assert_eq!(state.pending(), "ky");

    assert!(state.backspace());
    assert_eq!(state.pending(), "k");
    assert!(state.backspace());
    assert_eq!(state.pending(), "");
    // Nothing pending: the backspace belongs to the emitted kana instead.
    assert!(!state.backspace());
}

#[test]
fn clear_discards_pending_without_emitting() {
    let table = builtin();
    let mut state = Input::new();
    let mut out = String::new();
    table.feed(&mut state, 'k', &mut out).expect("String sink");
    state.clear();
    table.flush(&mut state, &mut out).expect("String sink");
    assert_eq!(out, "");
}

#[test]
fn a_full_sink_reports_overflow_instead_of_truncating_silently() {
    let table = builtin();
    let mut state = Input::new();
    // 3 bytes per kana: two fit, the third does not.
    let mut out = FixedStr::<6>::new();
    table.feed(&mut state, 'a', &mut out).expect("fits");
    table.feed(&mut state, 'i', &mut out).expect("fits");
    assert_eq!(table.feed(&mut state, 'u', &mut out), Err(Overflow));
    assert_eq!(out.as_str(), "あい");
}

/// EVAL: when `feed` resolves more than one step in the same call (here,
/// `n` completing to ん and then the fresh `q` starting a new,
/// still-pending candidate) and the sink overflows partway through, the
/// step(s) that already reached the sink must stay committed and the
/// step that didn't must stay recoverable. This is `drive`'s
/// `*candidate = next` ordering under test: it only runs after the sink
/// accepts the emission, so a failed emission leaves `candidate` (and
/// thus `state.pending` once `feed` stores it back) exactly where it was
/// before that step, not half-updated.
#[test]
fn overflow_preserves_unconsumed_suffix_after_partial_resolution() {
    let table = builtin();
    let mut state = Input::new();
    table
        .feed(&mut state, 'n', &mut String::new())
        .expect("String sink");

    // Exactly enough room for ん (3 bytes) and no more.
    let mut out = FixedStr::<3>::new();
    assert_eq!(table.feed(&mut state, 'q', &mut out), Err(Overflow));
    assert_eq!(out.as_str(), "ん", "the step that fit must still land");
    assert_eq!(
        state.pending(),
        "q",
        "the step that didn't fit must survive as pending, not vanish"
    );

    // A later flush to a sink with room recovers exactly the part that
    // overflowed -- nothing was lost, and nothing was emitted twice.
    let mut flushed = String::new();
    table.flush(&mut state, &mut flushed).expect("String sink");
    assert_eq!(flushed, "q");
}

/// EVAL: the non-ASCII branch of `feed` recovers differently from the
/// ASCII branch above, and that difference is worth spelling out rather
/// than leaving implicit. An ASCII overflow always leaves the
/// unconsumed part sitting in `state.pending` (proved above), because
/// pending romaji is where the FSM's mid-resolution state naturally
/// lives. A non-ASCII character never enters `pending` at all -- it
/// isn't ASCII, so the FSM's buffer cannot hold it even transiently --
/// so when `out.push(key)` overflows after a successful `flush`, the
/// character is not stored anywhere in `Input`. Recovery depends
/// entirely on the caller re-feeding the identical key once the sink has
/// room, exactly as `feed`'s doc comment describes ("a retry continues
/// from where it stopped"). This proves that retry actually works end to
/// end, and documents the asymmetry so a caller (`sakura-engine`'s
/// dispatch loop) cannot assume both overflow paths recover the same
/// way.
#[test]
fn non_ascii_overflow_never_loses_the_current_character() {
    let table = builtin();
    let mut state = Input::new();
    table
        .feed(&mut state, 'n', &mut String::new())
        .expect("String sink");

    // Exactly enough room for ん (3 bytes) and no more, so ん flushes
    // clean but 字 has nowhere to go.
    let mut out = FixedStr::<3>::new();
    assert_eq!(table.feed(&mut state, '字', &mut out), Err(Overflow));
    assert_eq!(out.as_str(), "ん");
    assert_eq!(
        state.pending(),
        "",
        "字 is not ASCII, so unlike the suffix case above it cannot be \
             held in Input's pending buffer -- there is nowhere in Input \
             for it to survive an overflow"
    );

    // Retrying the exact same key against a sink with room recovers it.
    let mut retry = String::new();
    table
        .feed(&mut state, '字', &mut retry)
        .expect("retry with room must succeed");
    assert_eq!(retry, "字");
}

/// EVAL: a corpus of real, whole words, each crossing several entry
/// boundaries. `every_carry_free_entry_is_reachable_by_typing_it` proves
/// every single entry works in isolation, but the `nn` report was never
/// about one entry in isolation -- it was about what happens where two
/// entries meet. This is the same idea applied at the boundary: sokuon
/// immediately followed by the consonant it carries back, youon next to
/// a plain vowel, a mapped run next to an unmapped character, and so on.
#[test]
fn whole_word_corpus_crosses_entry_boundaries_correctly() {
    let table = builtin();
    let cases = [
        ("ohayou", "おはよう"),
        ("arigatou", "ありがとう"),
        ("sayounara", "さようなら"),
        // sokuon immediately followed by the carried consonant + vowel.
        ("shuppatsu", "しゅっぱつ"),
        ("kekkon", "けっこん"),
        ("kippu", "きっぷ"),
        ("kitte", "きって"),
        ("zutto", "ずっと"),
        // a multi-char entry (`chi`) immediately followed by a plain
        // vowel that must not be absorbed into it.
        ("chiisai", "ちいさい"),
        // `s` alone is not a complete entry -- must wait for `sha`, not
        // misresolve partway through.
        ("kaisha", "かいしゃ"),
        // youon (`gyu`, `nyu`) next to a plain vowel and next to `n`.
        ("gyuunyuu", "ぎゅうにゅう"),
        // v-row, a mapped run next to an unmapped punctuation character.
        ("vaiorin", "ゔぁいおりん"),
        ("sugoi!", "すごい!"),
    ];
    for (input, expected) in cases {
        assert_eq!(committed(&table, input), expected, "input {input:?}");
    }
}

/// EVAL: the common word `反映` is typeable continuously as `hannei`, as
/// it is in Microsoft IME. The second `n` completes ん before `e`, rather
/// than becoming the `n` in ね and producing はんねい.
#[test]
fn hannei_can_be_typed_continuously_with_double_n() {
    let table = builtin();
    assert_eq!(committed(&table, "hannei"), "はんえい");
}

// --- Table compilation ---

/// A table of three entries is enough to exercise waiting, backtracking
/// and carry, which is the whole FSM.
#[test]
fn a_minimal_table_compiles_and_works() {
    let table =
        Table::parse("[kana]\nka = \"か\"\nki = \"き\"\nkk = [\"っ\", \"k\"]\n").expect("compile");
    assert_eq!(table.len(), 3);
    assert_eq!(committed(&table, "kakki"), "かっき");
    // `k` alone has no reading; it waits, then passes through on commit.
    assert_eq!(committed(&table, "k"), "k");
    // Nothing in this table reads `z`, so it goes straight out.
    assert_eq!(committed(&table, "zka"), "zか");
}

#[test]
fn every_malformed_table_names_its_fault() {
    let cases: [(&str, TableErrorKind); 8] = [
        ("[other]\na = \"あ\"\n", TableErrorKind::MissingSection),
        ("[kana]\n", TableErrorKind::EmptyTable),
        (
            "[kana]\n\"あ\" = \"あ\"\n",
            TableErrorKind::NonAsciiSequence,
        ),
        ("[kana]\nA = \"あ\"\n", TableErrorKind::UppercaseSequence),
        (
            "[kana]\nabcdefghi = \"あ\"\n",
            TableErrorKind::SequenceTooLong,
        ),
        ("[kana]\na = []\n", TableErrorKind::MalformedValue),
        ("[kana]\na = \"\"\n", TableErrorKind::EmptyEntry),
        (
            "[kana]\nkk = [\"っ\", \"kk\"]\n",
            TableErrorKind::CarryNotShorter,
        ),
    ];
    for (source, expected) in cases {
        let error = Table::parse(source).expect_err("expected a table error");
        assert_eq!(error.kind, expected, "source: {source:?}");
    }
}

#[test]
fn a_config_error_is_reported_as_one() {
    let error = Table::parse("[kana]\na = 1\n").expect_err("expected an error");
    assert!(matches!(error.kind, TableErrorKind::Config(_)));
    assert!(error.to_string().contains("line 2"));
}

/// The carry rule is what proves the resolution loop terminates, so it is
/// checked structurally rather than by hoping no user writes a cycle.
#[test]
fn a_self_referential_carry_is_rejected() {
    let error = Table::parse("[kana]\nab = [\"x\", \"ab\"]\n").expect_err("expected an error");
    assert_eq!(error.kind, TableErrorKind::CarryNotShorter);
}

/// The FSM eats whatever a keyboard can produce, in a process where a
/// panic is the host application dying. Termination is the property under
/// test: a run that does not stop hangs this test rather than failing it,
/// which is the correct outcome for an infinite loop.
#[test]
fn arbitrary_input_terminates_and_never_panics() {
    let table = builtin();
    // xorshift64*, because the workspace ships no third-party crates.
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };

    for _ in 0..20_000 {
        let length = (next() % 16) as usize;
        let mut input = Input::new();
        let mut out = String::new();
        for _ in 0..length {
            // The printable ASCII range plus a few characters outside it.
            let c = match next() % 32 {
                0 => 'あ',
                1 => '\u{3000}',
                n => char::from(0x20 + (n as u8 - 2) % 0x5F),
            };
            let _ = table.feed(&mut input, c, &mut out);
        }
        let _ = table.flush(&mut input, &mut out);
        assert!(input.is_empty(), "flush left {:?} pending", input.pending());
    }
}
