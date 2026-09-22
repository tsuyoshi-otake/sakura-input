use std::collections::{BTreeMap, VecDeque};

use super::{
    candidate_budget, CandidateAuthority, CandidateEvidence, CandidateOrigin, ConversionCandidate,
    ConversionInput, ConversionInputClass, ConversionOptions, Converter, CorrectionMap,
    CorrectionMapError, CorrectionRun, CrossCommitBridge, DictionaryEdgeBudget, LiteralPolicy,
    RawRepairBudget, RawRepairPlan, RepairTier, RightContextId, BASE_DICTIONARY_EDGES_PER_READING,
    MAX_CONVERSION_CANDIDATES, MAX_DICTIONARY_SURFACES_PER_READING, SINGLE_KANJI_ANNOTATION,
};
use crate::dictionary::{image_format, Dictionary, EntryFlags};
use crate::preferences::ConversionMethod;
use crate::user_dictionary::UserDictionary;
use crate::width::{CommaMark, PeriodMark, PunctuationStyle};
use crate::RepairKind;

#[derive(Clone)]
struct FixtureEntry {
    reading: String,
    surface: String,
    cost: i32,
    flags: EntryFlags,
}

#[derive(Default)]
struct FixtureTrieNode {
    label: char,
    children: BTreeMap<char, usize>,
    entries: Vec<usize>,
}

fn fixture_entry(reading: &str, surface: &str, cost: i32, flags: EntryFlags) -> FixtureEntry {
    FixtureEntry {
        reading: reading.to_owned(),
        surface: surface.to_owned(),
        cost,
        flags,
    }
}

#[test]
fn dictionary_edge_budget_preserves_baseline_rows_then_adds_surface_diversity() {
    let surface_bound = MAX_DICTIONARY_SURFACES_PER_READING as u32;
    let mut budget = DictionaryEdgeBudget::new();
    for _ in 0..BASE_DICTIONARY_EDGES_PER_READING {
        assert!(budget.admit(1), "the historical baseline rows must survive");
    }
    assert!(
        !budget.admit(1),
        "later POS rows must not consume diversity slots"
    );
    for surface_id in 2..=surface_bound {
        assert!(budget.admit(surface_id), "surface {surface_id}");
    }
    assert!(
        !budget.admit(surface_bound + 1),
        "the distinct-surface bound must remain finite"
    );

    budget.reset();
    assert!(
        budget.admit(surface_bound + 1),
        "a new reading gets a fresh budget"
    );
}

/// Issue #94: a path that opens with a bare one-character hiragana fragment
/// and then spends a whole kanji word is a splice of the reading, not a
/// parse of it, and those splices were sitting on the first candidate page.
/// The honorific prefixes stay, and so does a splice cheap enough to be a
/// real reading of the input.
#[test]
fn kana_fragment_prefix_splits_leave_the_candidate_page() {
    let rows = [
        fixture_entry("たいあん", "対案", 1000, EntryFlags::NONE),
        fixture_entry("た", "た", 100, EntryFlags::NONE),
        fixture_entry("いあん", "慰安", 2600, EntryFlags::NONE),
        fixture_entry("ごいけん", "御意見", 1000, EntryFlags::NONE),
        fixture_entry("ご", "ご", 100, EntryFlags::NONE),
        fixture_entry("いけん", "意見", 2600, EntryFlags::NONE),
        fixture_entry("とじょう", "途上", 1000, EntryFlags::NONE),
        fixture_entry("と", "と", 100, EntryFlags::NONE),
        fixture_entry("じょう", "場", 900, EntryFlags::NONE),
    ];
    let bytes = synthetic_dictionary(&rows);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let surfaces = |reading: &str| {
        let mut converter = Converter::new();
        converter
            .convert(&dictionary, reading, ConversionOptions::default())
            .expect("conversion")
            .iter()
            .map(|candidate| candidate.text().to_owned())
            .collect::<Vec<_>>()
    };

    let taian = surfaces("たいあん");
    assert!(taian.contains(&"対案".to_owned()), "{taian:?}");
    assert!(!taian.contains(&"た慰安".to_owned()), "{taian:?}");

    let goiken = surfaces("ごいけん");
    assert!(
        goiken.contains(&"ご意見".to_owned()),
        "an honorific prefix is a parse, not a splice: {goiken:?}"
    );

    let tojou = surfaces("とじょう");
    assert!(
        tojou.contains(&"と場".to_owned()),
        "a splice inside the window still spells a real word: {tojou:?}"
    );
}

/// Issue #94: the surface bound used to sit at the twelve baseline edges, so
/// the thirteenth distinct surface of a reading never entered the lattice at
/// all. Shipped きゅう spent its last slot on the rare name kanji 邱 and lost
/// the digit spelling 9 even though 9 had the cheaper whole-path cost. A
/// reading must expose as many distinct surfaces as the output frame carries.
#[test]
fn distinct_surfaces_beyond_the_baseline_edges_still_reach_conversion() {
    const BASELINE_SURFACES: [&str; 12] = [
        "旧", "級", "急", "給", "球", "究", "求", "九", "久", "休", "吸", "宮",
    ];
    assert_eq!(BASELINE_SURFACES.len(), BASE_DICTIONARY_EDGES_PER_READING);

    let mut rows = BASELINE_SURFACES
        .iter()
        .enumerate()
        .map(|(index, surface)| {
            fixture_entry("きゅう", surface, 100 + index as i32, EntryFlags::NONE)
        })
        .collect::<Vec<_>>();
    rows.push(fixture_entry("きゅう", "9", 200, EntryFlags::NONE));
    let bytes = synthetic_dictionary(&rows);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");

    for method in ConversionMethod::ALL {
        let mut converter = Converter::new();
        let candidates = converter
            .convert(
                &dictionary,
                "きゅう",
                ConversionOptions {
                    method,
                    ..ConversionOptions::default()
                },
            )
            .expect("conversion");
        assert!(
            candidates.iter().any(|candidate| candidate.text() == "9"),
            "{method:?}: the thirteenth surface must survive the edge budget: {candidates:?}"
        );
    }
}

#[test]
fn repeated_pos_rows_cannot_hide_a_distinct_surface_from_conversion() {
    let mut rows = (0..12)
        .map(|cost| fixture_entry("たて", "同じ", cost, EntryFlags::NONE))
        .collect::<Vec<_>>();
    rows.push(fixture_entry("たて", "縦", 100, EntryFlags::NONE));
    let bytes = synthetic_dictionary(&rows);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");

    for method in ConversionMethod::ALL {
        let mut converter = Converter::new();
        let candidates = converter
            .convert(
                &dictionary,
                "たて",
                ConversionOptions {
                    method,
                    ..ConversionOptions::default()
                },
            )
            .expect("conversion");
        assert!(
            candidates.iter().any(|candidate| candidate.text() == "縦"),
            "{method:?}: {candidates:?}"
        );
    }
}

/// A one-mora reading is where the gap against a commercial IME is widest
/// and where the ranked list runs out first. The fixture therefore uses
/// one ranked entry and a character list that overlaps it.
fn single_kanji_fixture() -> Vec<u8> {
    synthetic_dictionary_with_single_kanji(
        &[
            fixture_entry("ひ", "日", 100, EntryFlags::NONE),
            fixture_entry("ひかり", "光", 100, EntryFlags::NONE),
        ],
        // Byte-ascending readings, each listing characters in the source's
        // own preference order. 日 is deliberately also a ranked entry.
        &[("ひ", "日火比髙"), ("ひかり", "灯")],
        &[('髙', '高', 1)],
    )
}

fn converted(bytes: &[u8], reading: &str, wanted: usize) -> Vec<(String, String)> {
    let dictionary = Dictionary::parse(bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();
    converter
        .convert(
            &dictionary,
            reading,
            ConversionOptions {
                max_candidates: wanted,
                ..ConversionOptions::default()
            },
        )
        .expect("conversion")
        .iter()
        .map(|candidate| {
            (
                candidate.text().to_string(),
                candidate.annotation().to_string(),
            )
        })
        .collect()
}

#[test]
fn single_kanji_fill_the_slots_the_ranked_list_left_empty() {
    let listed = converted(&single_kanji_fixture(), "ひ", 8);
    let surfaces = listed
        .iter()
        .map(|(text, _)| text.as_str())
        .collect::<Vec<_>>();
    // 日 is ranked, so it keeps its ranked position and is not repeated in
    // the tail; the rest follow in the source's preference order.
    assert_eq!(surfaces.first(), Some(&"日"));
    assert_eq!(&surfaces[surfaces.len() - 3..], &["火", "比", "髙"]);
    assert_eq!(surfaces.iter().filter(|text| **text == "日").count(), 1);
}

#[test]
fn single_kanji_remain_reachable_after_a_saturated_search() {
    let listed: String = (0x4e00..0x4e00 + 315).filter_map(char::from_u32).collect();
    let rows: Vec<_> = (0..256)
        .map(|index| fixture_entry("こう", &format!("候補{index}"), index, EntryFlags::NONE))
        .collect();
    let bytes = synthetic_dictionary_with_single_kanji(&rows, &[("こう", &listed)], &[]);
    let dictionary = Dictionary::parse(&bytes).unwrap();
    let mut converter = Converter::new();
    let result = converter
        .convert_detailed(&dictionary, "こう", ConversionOptions::default())
        .unwrap();
    assert_eq!(result.candidates()[0].text(), "候補0");
    for character in listed.chars() {
        assert!(
            result
                .candidates()
                .iter()
                .any(|c| c.text() == character.to_string()),
            "missing {character}"
        );
    }
    assert!(result.candidates().len() > 256);
}

#[test]
fn long_single_kanji_reading_keeps_its_tail_without_widening_search() {
    let reading = "まいくろりっとる";
    let rows: Vec<_> = (0..108)
        .map(|index| fixture_entry(reading, &format!("候補{index}"), index, EntryFlags::NONE))
        .collect();
    let bytes = synthetic_dictionary_with_single_kanji(&rows, &[(reading, "竗")], &[]);
    let listed = converted(&bytes, reading, MAX_CONVERSION_CANDIDATES);
    assert!(listed.iter().any(|(text, _)| text == "竗"));
    assert_eq!(listed.len(), candidate_budget(reading) + 1);
}

#[test]
fn the_appended_tail_never_changes_the_ranked_list() {
    let rows = [
        fixture_entry("ひ", "日", 100, EntryFlags::NONE),
        fixture_entry("ひかり", "光", 100, EntryFlags::NONE),
    ];
    let without = converted(&synthetic_dictionary(&rows), "ひ", 8);
    let with = converted(&single_kanji_fixture(), "ひ", 8);
    assert!(with.len() > without.len(), "the tail must add rows");
    assert_eq!(
        &with[..without.len()],
        &without[..],
        "every ranked row must keep its text, annotation, and position"
    );
}

#[test]
fn the_tail_stops_at_the_candidate_limit() {
    for wanted in 1..=8 {
        let listed = converted(&single_kanji_fixture(), "ひ", wanted);
        assert!(
            listed.len() <= wanted,
            "limit {wanted} produced {} candidates",
            listed.len()
        );
    }
    // The limit, not the character list, is what stops the tail: one slot
    // leaves room for the ranked entry alone.
    assert_eq!(converted(&single_kanji_fixture(), "ひ", 1).len(), 1);
}

#[test]
fn mcc_mcdc_single_kanji_tail_guard() {
    for (full, absent, skipped) in [
        (false, false, false),
        (false, true, true),
        (true, false, true),
        (true, true, true),
    ] {
        let bytes = if absent {
            synthetic_dictionary(&[fixture_entry("ひ", "日", 100, EntryFlags::NONE)])
        } else {
            single_kanji_fixture()
        };
        let dictionary = Dictionary::parse(&bytes).unwrap();
        let mut converter = Converter::new();
        converter
            .convert(
                &dictionary,
                "ひ",
                ConversionOptions {
                    max_candidates: 1,
                    ..ConversionOptions::default()
                },
            )
            .unwrap();
        assert_eq!(converter.candidates.len(), 1);
        assert_eq!(converter.candidates[0].text(), "日");
        converter
            .append_single_kanji(&dictionary, "ひ", if full { 1 } else { 8 })
            .unwrap();
        assert_eq!(converter.candidates.len() == 1, skipped);
        assert_eq!(converter.candidates[0].text(), "日");
        if !skipped {
            assert!(converter
                .candidates
                .iter()
                .any(|candidate| candidate.text() == "火"));
        }
        println!(
            "decision-evidence core.single_kanji_guard {}{} {}",
            u8::from(full),
            u8::from(absent),
            u8::from(skipped)
        );
    }
}

/// Issue #95: far more single-kanji characters than the pre-#95 ceiling
/// of 18, so a test can tell whether a wide request actually reached the
/// tail or was silently narrowed by `candidate_budget`. The ranked row's
/// surface is deliberately also the first listed character, the same
/// overlap `single_kanji_fixture` exercises above, so the count reflects
/// distinct surfaces rather than a duplicate.
fn dictionary_with_many_single_kanji(reading: &str) -> Vec<u8> {
    const LISTED: &str = "日月火水木金土人子女男大小上下中左右前後内外一二三四五六七八九十";
    debug_assert_eq!(LISTED.chars().count(), 32);
    synthetic_dictionary_with_single_kanji(
        &[fixture_entry(reading, "日", 100, EntryFlags::NONE)],
        &[(reading, LISTED)],
        &[],
    )
}

/// Issue #95: readings are kana, so a byte count must not stand in for a
/// character count. Every reading below is multi-byte UTF-8, so a byte
/// count would already have crossed a boundary a character count has
/// not, and this would catch that mistake as a wrong tier rather than a
/// panic.
#[test]
fn candidate_budget_switches_tiers_at_four_and_eight_characters() {
    assert_eq!(candidate_budget(&"あ".repeat(4)), 256);
    assert_eq!(candidate_budget(&"あ".repeat(5)), 108);
    assert_eq!(candidate_budget(&"あ".repeat(8)), 108);
    assert_eq!(candidate_budget(&"あ".repeat(9)), 18);
}

/// Issue #95 raised `MAX_CONVERSION_CANDIDATES` from 18 to 256
/// specifically so a short reading could reach single-kanji surfaces the
/// old ceiling trimmed away; `candidate_budget` must actually let them
/// through instead of silently keeping the old limit.
#[test]
fn a_short_reading_can_receive_more_than_eighteen_candidates_now() {
    let bytes = dictionary_with_many_single_kanji("ひ");
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(&dictionary, "ひ", ConversionOptions::default())
        .expect("conversion");
    assert!(
        candidates.len() > 18,
        "a one-character reading's own budget is the full ceiling: {} candidates",
        candidates.len()
    );
}

/// A long reading's candidate list is whole-sentence parses nobody pages
/// through -- Issue #95 measured no single-kanji or homophone benefit
/// past eight characters -- so it keeps the pre-#95 ceiling even when
/// the caller explicitly asks for the full 256.
#[test]
fn a_long_reading_search_still_holds_at_eighteen_even_at_the_full_ceiling() {
    let long_reading = "ひ".repeat(9);
    let rows: Vec<_> = (0..64)
        .map(|index| {
            fixture_entry(
                &long_reading,
                &format!("候補{index}"),
                index,
                EntryFlags::NONE,
            )
        })
        .collect();
    let bytes = synthetic_dictionary(&rows);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(
            &dictionary,
            &long_reading,
            ConversionOptions {
                max_candidates: MAX_CONVERSION_CANDIDATES,
                ..ConversionOptions::default()
            },
        )
        .expect("conversion");
    assert_eq!(
        candidates.len(),
        18,
        "a long reading must not see past the pre-#95 ceiling: {candidates:?}"
    );
}

/// The clamp only ever narrows a request; it must never raise a
/// caller's own smaller number back up to the short-reading ceiling.
#[test]
fn a_smaller_request_than_the_budget_keeps_its_own_number() {
    let bytes = dictionary_with_many_single_kanji("ひ");
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(
            &dictionary,
            "ひ",
            ConversionOptions {
                max_candidates: 5,
                ..ConversionOptions::default()
            },
        )
        .expect("conversion");
    assert_eq!(
        candidates.len(),
        5,
        "the short-reading budget of 256 must not override a smaller request: {candidates:?}"
    );
}

#[test]
fn an_appended_character_carries_its_relation_or_a_plain_marker() {
    let listed = converted(&single_kanji_fixture(), "ひ", 8);
    let note = |surface: &str| {
        listed
            .iter()
            .find(|(text, _)| text == surface)
            .map(|(_, annotation)| annotation.clone())
            .unwrap_or_else(|| panic!("{surface} is missing"))
    };
    assert_eq!(note("髙"), "異体字（高）");
    assert_eq!(note("火"), "単漢字");
    assert_eq!(note("比"), "単漢字");
}

#[test]
fn a_reading_the_table_does_not_list_gets_no_tail() {
    // ひか sits between ひ and ひかり, so a lookup that stopped at the
    // shorter length would hand it one of their character lists.
    for reading in ["ひか", "ひかりの"] {
        let listed = converted(&single_kanji_fixture(), reading, 8);
        assert!(
            listed.iter().all(|(_, annotation)| annotation != "単漢字"),
            "{reading} borrowed a character list: {listed:?}"
        );
    }
}

#[test]
fn appended_costs_stay_above_every_ranked_cost() {
    let bytes = single_kanji_fixture();
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();
    let result = converter
        .convert_detailed(
            &dictionary,
            "ひ",
            ConversionOptions {
                max_candidates: 8,
                ..ConversionOptions::default()
            },
        )
        .expect("conversion");
    let candidates = result.candidates();
    let appended = |candidate: &ConversionCandidate| {
        candidate.annotation() == SINGLE_KANJI_ANNOTATION
            || candidate.annotation().starts_with("異体字")
    };
    let ranked_ceiling = candidates
        .iter()
        .filter(|candidate| !appended(candidate))
        .map(|candidate| candidate.cost)
        .max()
        .expect("a ranked row exists");
    let tail = candidates
        .iter()
        .filter(|candidate| appended(candidate))
        .collect::<Vec<_>>();
    assert!(!tail.is_empty(), "the fixture must produce a tail");
    // A later re-sort by cost must not be able to lift the tail into the
    // ranked list.
    for candidate in tail {
        assert!(
            candidate.cost > ranked_ceiling,
            "{} costs {} at or below the ranked ceiling {ranked_ceiling}",
            candidate.text(),
            candidate.cost
        );
    }
}

/// Issue #99.  The setting picks which mark comes first; it never picks
/// which marks exist.  Every one of the nine combinations has to reach all
/// four members of the family it is asked for, in an order it decides.
#[test]
fn every_punctuation_style_offers_its_whole_family_configured_glyph_first() {
    let bytes = synthetic_dictionary(&[fixture_entry("ひ", "日", 100, EntryFlags::NONE)]);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();
    for comma in CommaMark::ALL {
        for period in PeriodMark::ALL {
            let style = PunctuationStyle::new(comma, period);
            let roles = [('\u{3001}', comma.glyph()), ('\u{3002}', period.glyph())];
            for (reading, configured) in roles {
                let mut source = String::new();
                source.push(reading);
                let expected = style
                    .family_for(reading)
                    .expect("a punctuation reading belongs to a family");
                let result = converter
                    .convert_detailed(
                        &dictionary,
                        &source,
                        ConversionOptions {
                            punctuation: style,
                            ..ConversionOptions::default()
                        },
                    )
                    .expect("conversion");
                let candidates = result.candidates();
                assert!(
                    candidates.len() >= expected.len(),
                    "{style:?} on {reading:?} produced only {} candidates",
                    candidates.len()
                );
                for (offset, variant) in expected.into_iter().enumerate() {
                    let candidate = &candidates[offset];
                    assert_eq!(
                        candidate.text(),
                        variant.glyph.to_string(),
                        "{style:?} on {reading:?} at slot {offset}"
                    );
                    assert_eq!(candidate.annotation(), variant.annotation);
                    // Without this bit the choke point rewrites all four
                    // rows to the configured glyph and the page shows one
                    // character four times.
                    assert!(
                        candidate.is_synthetic_exact(),
                        "{style:?} on {reading:?}: slot {offset} would be re-styled"
                    );
                }
                assert_eq!(
                    candidates[0].text(),
                    configured.to_string(),
                    "{style:?} must still default to the mark the reader set"
                );
                // A family member appearing twice -- once appended, once
                // left over from the search -- is the failure this guards.
                let members = candidates
                    .iter()
                    .filter(|candidate| {
                        expected
                            .iter()
                            .any(|variant| candidate.text() == variant.glyph.to_string())
                    })
                    .count();
                assert_eq!(
                    members,
                    expected.len(),
                    "{style:?} on {reading:?} listed a family member more than once"
                );
            }
        }
    }
}

/// The family rewrites one reading list, not the ranking.  A reading that
/// is not a punctuation mark has to convert identically under every
/// setting, or the feature has moved ordinary candidates around.
#[test]
fn an_ordinary_reading_converts_identically_under_every_punctuation_style() {
    let bytes = single_kanji_fixture();
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();
    let listed = |converter: &mut Converter, style: PunctuationStyle| {
        converter
            .convert_detailed(
                &dictionary,
                "ひ",
                ConversionOptions {
                    punctuation: style,
                    max_candidates: 8,
                    ..ConversionOptions::default()
                },
            )
            .expect("conversion")
            .candidates()
            .iter()
            .map(|candidate| (candidate.text().to_owned(), candidate.cost))
            .collect::<Vec<_>>()
    };
    let baseline = listed(&mut converter, PunctuationStyle::default());
    assert!(!baseline.is_empty(), "the fixture must convert");
    for comma in CommaMark::ALL {
        for period in PeriodMark::ALL {
            let style = PunctuationStyle::new(comma, period);
            assert_eq!(listed(&mut converter, style), baseline, "{style:?}");
        }
    }
}

/// A reading that merely contains a punctuation mark is an ordinary
/// sentence, not a request for the family.
#[test]
fn only_a_reading_that_is_itself_one_mark_opens_the_family() {
    let bytes = synthetic_dictionary(&[fixture_entry("ひ", "日", 100, EntryFlags::NONE)]);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();
    let style = PunctuationStyle::new(CommaMark::HalfWidth, PeriodMark::HalfWidth);
    for reading in ["\u{3001}ひ", "ひ\u{3001}", "\u{3001}\u{3001}", "ひ"] {
        let candidates = converter
            .convert_detailed(
                &dictionary,
                reading,
                ConversionOptions {
                    punctuation: style,
                    ..ConversionOptions::default()
                },
            )
            .expect("conversion")
            .candidates()
            .iter()
            .map(|candidate| candidate.text().to_owned())
            .collect::<Vec<_>>();
        assert!(
            !candidates.iter().any(|text| text == ","),
            "{reading:?} must not be offered the comma family: {candidates:?}"
        );
    }
}

#[test]
fn an_image_without_the_tables_converts_exactly_as_before() {
    let rows = [fixture_entry("ひ", "日", 100, EntryFlags::NONE)];
    let listed = converted(&synthetic_dictionary(&rows), "ひ", 8);
    assert!(
        listed.iter().all(|(_, annotation)| annotation != "単漢字"),
        "{listed:?}"
    );
}

/// Small in-test image writer.  Keeping it here avoids adding the
/// allocating `dictc` crate as a sakura-core dependency while still
/// exercising the borrowed production dictionary parser and lattice.
fn synthetic_dictionary(rows: &[FixtureEntry]) -> Vec<u8> {
    synthetic_image(rows, Vec::new())
}

/// Same image writer, plus the optional single-kanji tables.
///
/// `single_kanji` pairs a reading with the characters it lists, and
/// `variants` relates a character to another with a rule code. Both must
/// already be in the ascending order the reader binary-searches, exactly
/// as `dictc` emits them.
fn synthetic_dictionary_with_single_kanji(
    rows: &[FixtureEntry],
    single_kanji: &[(&str, &str)],
    variants: &[(char, char, u8)],
) -> Vec<u8> {
    let mut index = Vec::new();
    let mut reading_data: Vec<u8> = Vec::new();
    let mut characters: Vec<u8> = Vec::new();
    let mut character_total = 0usize;
    for (reading, listed) in single_kanji {
        put_u32(&mut index, reading_data.len() as u32);
        put_u32(&mut index, character_total as u32);
        put_u16(&mut index, reading.len() as u16);
        put_u16(&mut index, listed.chars().count() as u16);
        reading_data.extend_from_slice(reading.as_bytes());
        for character in listed.chars() {
            put_u32(&mut characters, character as u32);
            character_total += 1;
        }
    }
    let mut variant_data = Vec::new();
    for (variant, original, kind) in variants {
        put_u32(&mut variant_data, *variant as u32);
        put_u32(&mut variant_data, *original as u32);
        variant_data.push(*kind);
        variant_data.extend_from_slice(&[0, 0, 0]);
    }
    let reading_bytes = reading_data.len();
    synthetic_image(
        rows,
        vec![
            (
                image_format::TAG_SINGLE_KANJI_INDEX,
                index,
                single_kanji.len(),
            ),
            (
                image_format::TAG_SINGLE_KANJI_READINGS,
                reading_data,
                reading_bytes,
            ),
            (
                image_format::TAG_SINGLE_KANJI_CHARS,
                characters,
                character_total,
            ),
            (
                image_format::TAG_SINGLE_KANJI_VARIANTS,
                variant_data,
                variants.len(),
            ),
        ],
    )
}

/// `extra` holds already-encoded optional tables, appended to the
/// directory in the order the caller lists them.
fn synthetic_image(rows: &[FixtureEntry], extra: Vec<([u8; 4], Vec<u8>, usize)>) -> Vec<u8> {
    let mut rows = rows.to_vec();
    rows.sort_by(|left, right| {
        (&left.reading, &left.surface, left.cost).cmp(&(&right.reading, &right.surface, right.cost))
    });

    let mut trie = vec![FixtureTrieNode {
        label: '\0',
        ..FixtureTrieNode::default()
    }];
    for (entry_index, row) in rows.iter().enumerate() {
        let mut node = 0usize;
        for character in row.reading.chars() {
            let child = if let Some(child) = trie[node].children.get(&character).copied() {
                child
            } else {
                let child = trie.len();
                trie.push(FixtureTrieNode {
                    label: character,
                    ..FixtureTrieNode::default()
                });
                trie[node].children.insert(character, child);
                child
            };
            node = child;
        }
        trie[node].entries.push(entry_index);
    }

    let mut order = Vec::with_capacity(trie.len());
    let mut queue = VecDeque::from([0usize]);
    while let Some(old) = queue.pop_front() {
        order.push(old);
        queue.extend(trie[old].children.values().copied());
    }
    let mut old_to_new = vec![0usize; trie.len()];
    for (new, old) in order.iter().copied().enumerate() {
        old_to_new[old] = new;
    }

    let node_count = order.len();
    let louds_bits = node_count * 2 - 1;
    let mut louds = vec![0u8; 4 + louds_bits.div_ceil(8)];
    put_u32_at(&mut louds, 0, louds_bits as u32);
    let mut bit = 0usize;
    let mut nodes = Vec::with_capacity(node_count * image_format::NODE_LEN);
    let mut labels = Vec::with_capacity(node_count * 4);
    for old in order.iter().copied() {
        let node = &trie[old];
        let first_child = node
            .children
            .values()
            .next()
            .map(|child| old_to_new[*child])
            .unwrap_or(0);
        put_u32(&mut nodes, first_child as u32);
        put_u16(&mut nodes, node.children.len() as u16);
        put_u16(&mut nodes, node.entries.len() as u16);
        put_u32(
            &mut nodes,
            node.entries.first().copied().unwrap_or(0) as u32,
        );
        put_u32(&mut nodes, 0);
        put_u32(&mut labels, node.label as u32);
        for _ in &node.children {
            louds[4 + bit / 8] |= 1 << (bit % 8);
            bit += 1;
        }
        bit += 1;
    }

    let mut surfaces = rows
        .iter()
        .map(|row| row.surface.clone())
        .collect::<Vec<_>>();
    surfaces.sort();
    surfaces.dedup();
    let mut surface_offsets = Vec::with_capacity(surfaces.len() * 4);
    let mut surface_data = Vec::new();
    for surface in &surfaces {
        put_u32(&mut surface_offsets, surface_data.len() as u32);
        put_u16(&mut surface_data, 0);
        put_u16(&mut surface_data, surface.len() as u16);
        surface_data.extend_from_slice(surface.as_bytes());
    }

    let mut entries = Vec::with_capacity(rows.len() * image_format::ENTRY_LEN);
    for row in &rows {
        let surface_id = surfaces
            .binary_search(&row.surface)
            .expect("fixture surface") as u32;
        put_u32(&mut entries, surface_id);
        put_u16(&mut entries, 0);
        put_u16(&mut entries, 0);
        put_i32(&mut entries, row.cost);
        put_i32(&mut entries, i32::MAX);
        put_u16(&mut entries, row.flags.bits());
        put_u16(&mut entries, 0);
        put_u32(&mut entries, image_format::NO_ANNOTATION);
    }

    let mut matrix = Vec::new();
    matrix.extend_from_slice(&image_format::MATRIX_MAGIC);
    put_u16(&mut matrix, 1);
    put_u16(&mut matrix, 0);
    put_u32(&mut matrix, 0);
    put_u32(&mut matrix, 0);
    put_u16(&mut matrix, 0);
    matrix.resize(20, 0);
    put_u32(&mut matrix, 0);
    put_u32(&mut matrix, 0);

    let mut tables = vec![
        (image_format::TAG_LOUDS, louds, louds_bits),
        (image_format::TAG_NODES, nodes, node_count),
        (image_format::TAG_LABELS, labels, node_count),
        (image_format::TAG_ENTRIES, entries, rows.len()),
        (
            image_format::TAG_SURFACE_OFFSETS,
            surface_offsets,
            surfaces.len(),
        ),
        (image_format::TAG_SURFACES, surface_data, surfaces.len()),
        (image_format::TAG_ANNOTATION_OFFSETS, Vec::new(), 0),
        (image_format::TAG_ANNOTATIONS, Vec::new(), 0),
        (image_format::TAG_MATRIX, matrix, 1),
    ];
    tables.extend(extra);
    let prefix = image_format::HEADER_LEN + tables.len() * image_format::DIRECTORY_ENTRY_LEN;
    let mut image = vec![0u8; prefix];
    let mut directory = Vec::with_capacity(tables.len());
    for (tag, bytes, count) in tables {
        while !image.len().is_multiple_of(8) {
            image.push(0);
        }
        let offset = image.len();
        image.extend_from_slice(&bytes);
        directory.push((tag, offset, bytes.len(), count));
    }
    image[0..8].copy_from_slice(&image_format::MAGIC);
    put_u16_at(&mut image, 8, image_format::VERSION);
    put_u16_at(&mut image, 10, image_format::HEADER_LEN as u16);
    put_u16_at(&mut image, 12, directory.len() as u16);
    put_u16_at(&mut image, 14, 1);
    put_u32_at(&mut image, 16, rows.len() as u32);
    put_u32_at(&mut image, 20, node_count as u32);
    let image_len = image.len() as u32;
    put_u32_at(&mut image, 24, image_len);
    put_u32_at(&mut image, 28, 0);
    for (index, (tag, offset, len, count)) in directory.into_iter().enumerate() {
        let at = image_format::HEADER_LEN + index * image_format::DIRECTORY_ENTRY_LEN;
        image[at..at + 4].copy_from_slice(&tag);
        put_u32_at(&mut image, at + 4, offset as u32);
        put_u32_at(&mut image, at + 8, len as u32);
        put_u32_at(&mut image, at + 12, count as u32);
    }
    image
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_i32(out: &mut Vec<u8>, value: i32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u16_at(out: &mut [u8], at: usize, value: u16) {
    out[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32_at(out: &mut [u8], at: usize, value: u32) {
    out[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn local_plan(plan_id: u8, original: &str, corrected: &str, tier: RepairTier) -> RawRepairPlan {
    let runs = [CorrectionRun::replace(
        0,
        u16::try_from(corrected.len()).expect("corrected length"),
        0,
        u16::try_from(original.len()).expect("original length"),
    )];
    let map = CorrectionMap::new(original, corrected, &runs).expect("map");
    RawRepairPlan::new(plan_id, corrected, map, tier).expect("plan")
}

#[test]
fn correction_map_projects_only_forward_complete_boundaries() {
    let original = "ないkにいく";
    let corrected = "ないかにいく";
    let runs = [
        CorrectionRun::equal(0, 6, 0, 6),
        CorrectionRun::replace(6, 9, 6, 7),
        CorrectionRun::equal(9, 18, 7, 16),
    ];
    let map = CorrectionMap::new(original, corrected, &runs).expect("valid map");
    assert_eq!(map.project_corrected_range(0, 12), Some((0, 10)));
    assert_eq!(map.project_corrected_range(12, 18), Some((10, 16)));
    assert_eq!(map.project_corrected_range(7, 12), None);
    assert_eq!(map.project_corrected_range(0, 18), Some((0, 16)));
    assert_eq!(map.project_corrected_range(1, 3), None);
}

#[test]
fn correction_map_rejects_mismatched_snapshot_and_invalid_runs() {
    let original = "ないkにいく";
    let corrected = "ないかにいく";
    let runs = [
        CorrectionRun::equal(0, 6, 0, 6),
        CorrectionRun::replace(6, 9, 6, 7),
        CorrectionRun::equal(9, 18, 7, 16),
    ];
    let map = CorrectionMap::new(original, corrected, &runs).expect("valid map");
    assert_eq!(
        map.validate_for_readings("ないkにいる", corrected),
        Err(CorrectionMapError::EqualRunMismatch)
    );
    assert_eq!(
        CorrectionMap::new(original, corrected, &[CorrectionRun::replace(0, 6, 0, 6)]),
        Err(CorrectionMapError::ReplaceRunUnchanged)
    );
    let replacement_only = CorrectionMap::new(
        "あき",
        "あか",
        &[CorrectionRun::replace(
            0,
            "あか".len() as u16,
            0,
            "あき".len() as u16,
        )],
    )
    .expect("replacement-only map");
    assert_eq!(
        replacement_only.validate_for_readings("いき", "あか"),
        Err(CorrectionMapError::SnapshotMismatch)
    );
}

#[test]
fn candidate_authority_is_strictly_direct_then_local_then_general() {
    assert!(CandidateAuthority::Direct.rank() > CandidateAuthority::LocalRawCompletion.rank());
    assert!(
        CandidateAuthority::LocalRawCompletion.rank()
            > CandidateAuthority::GeneralSingleInsertion.rank()
    );
    assert_eq!(
        CandidateOrigin::RawRepair {
            plan_id: 1,
            tier: RepairTier::LocalCompletion,
        }
        .authority(),
        CandidateAuthority::LocalRawCompletion
    );
}

#[test]
fn derived_repair_evidence_stays_outside_neural_lexical_groups() {
    for kind in [
        RepairKind::Rule,
        RepairKind::Advanced,
        RepairKind::CommitHistory,
        RepairKind::EnglishSpelling,
    ] {
        let evidence = CandidateEvidence::Repair(kind);
        assert_eq!(evidence.neural_group(), None, "{kind:?}");
        assert!(!evidence.is_trustworthy_whole_reading_exact());
    }
    assert_eq!(CandidateEvidence::Generated.neural_group(), None);
    assert_eq!(CandidateEvidence::Fallback.neural_group(), None);
    assert_eq!(
        CandidateEvidence::RawRepair {
            tier: RepairTier::GeneralSingleInsertion,
        }
        .neural_group(),
        None
    );
}

#[test]
fn candidate_evidence_keeps_exact_and_composite_homophones_distinct() {
    let bytes = synthetic_dictionary(&[
        fixture_entry("ろうりょく", "労力", 0, EntryFlags::NONE),
        fixture_entry("ろう", "ロウ", 1, EntryFlags::NONE),
        fixture_entry("りょく", "力", 1, EntryFlags::NONE),
    ]);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(&dictionary, "ろうりょく", ConversionOptions::default())
        .expect("homophone conversion");

    let exact = candidates
        .iter()
        .find(|candidate| candidate.text() == "労力")
        .expect("whole-reading exact candidate");
    assert_eq!(exact.evidence_class(), CandidateEvidence::ExactSystem);
    assert!(exact.is_trustworthy_whole_reading_exact());

    let composite = candidates
        .iter()
        .find(|candidate| candidate.text() == "ロウ力")
        .expect("composite lexical homophone");
    assert_eq!(
        composite.evidence_class(),
        CandidateEvidence::CompositeLexical
    );
    assert!(!composite.is_trustworthy_whole_reading_exact());
    assert_ne!(exact.evidence_class(), composite.evidence_class());
}

#[test]
fn candidate_evidence_preserves_commit_history_repair_boundary() {
    let bytes = cross_commit_fixture();
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();
    converter.set_commit_repair_readings(&["もれないか"]);
    let candidates = converter
        .convert(&dictionary, "ないか", ConversionOptions::default())
        .expect("commit-history conversion");

    let repaired = candidates
        .iter()
        .find(|candidate| candidate.text() == "漏れないか")
        .expect("commit-history candidate");
    assert_eq!(
        repaired.evidence_class(),
        CandidateEvidence::Repair(RepairKind::CommitHistory)
    );
    assert!(!repaired.is_trustworthy_whole_reading_exact());

    let exact = candidates
        .iter()
        .find(|candidate| candidate.text() == "内科")
        .expect("ordinary exact candidate");
    assert_eq!(exact.evidence_class(), CandidateEvidence::ExactSystem);
    assert!(exact.is_trustworthy_whole_reading_exact());
}

fn cross_commit_fixture() -> Vec<u8> {
    synthetic_dictionary(&[
        fixture_entry("もれ", "漏れ", 0, EntryFlags::NONE),
        fixture_entry("ないか", "内科", 50, EntryFlags::NONE),
        fixture_entry("ないか", "内か", 70, EntryFlags::NONE),
        fixture_entry("ないか", "ないか", 100, EntryFlags::NONE),
        fixture_entry("ないか", "無いか", 110, EntryFlags::NONE),
        // These whole-reading entries are the evidence that cannot exist
        // in a lattice beginning at the current reading's byte zero.
        fixture_entry("もれないか", "漏れないか", 10, EntryFlags::NONE),
    ])
}

fn issue_83_bridge<'a>() -> CrossCommitBridge<'a> {
    CrossCommitBridge {
        tail_reading: "もれ",
        tail_surface: "漏れ",
        prefix_right_id: RightContextId::new(0),
        prefix_cost: 0,
    }
}

#[test]
fn cross_commit_bridge_rescores_only_reachable_current_candidates() {
    let bytes = cross_commit_fixture();
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();

    let baseline = converter
        .convert(&dictionary, "ないか", ConversionOptions::default())
        .expect("baseline conversion");
    assert_eq!(baseline[0].text(), "内科");

    let result = converter
        .convert_with_user_dictionary_input_bridge_detailed(
            &dictionary,
            None,
            ConversionInput::ordinary("ないか"),
            ConversionOptions::default(),
            Some(issue_83_bridge()),
        )
        .expect("bridged conversion");
    let surfaces: Vec<&str> = result
        .candidates()
        .iter()
        .map(|candidate| candidate.text())
        .collect();
    let plain = surfaces
        .iter()
        .position(|surface| *surface == "ないか")
        .unwrap();
    let negative = surfaces
        .iter()
        .position(|surface| *surface == "無いか")
        .unwrap();
    let clinic = surfaces
        .iter()
        .position(|surface| *surface == "内科")
        .unwrap();
    let mixed = surfaces
        .iter()
        .position(|surface| *surface == "内か")
        .unwrap();
    assert!(plain < clinic && negative < clinic, "{surfaces:?}");
    assert!(
        clinic < mixed,
        "one-kana overlap must not inherit the bridge: {surfaces:?}"
    );
    assert_eq!(result.candidates()[plain].segments()[0].reading_start, 0);
    assert_eq!(
        usize::from(
            result.candidates()[plain]
                .segments()
                .last()
                .unwrap()
                .reading_end
        ),
        "ないか".len()
    );
    let diagnostics = result.diagnostics();
    assert!(diagnostics.cross_commit_bridge_attempted);
    assert_eq!(diagnostics.cross_commit_bridge_candidates_rescored, 2);
}

#[test]
fn cross_commit_bridge_is_lexeme_agnostic() {
    let bytes = synthetic_dictionary(&[
        fixture_entry("けんとう", "検討", 0, EntryFlags::NONE),
        fixture_entry("しますか", "シマスカ", 50, EntryFlags::NONE),
        fixture_entry("しますか", "しますか", 100, EntryFlags::NONE),
        fixture_entry("けんとうしますか", "検討しますか", 10, EntryFlags::NONE),
    ]);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();

    let baseline = converter
        .convert(&dictionary, "しますか", ConversionOptions::default())
        .expect("baseline conversion");
    assert_eq!(baseline[0].text(), "シマスカ");

    let result = converter
        .convert_with_user_dictionary_input_bridge_detailed(
            &dictionary,
            None,
            ConversionInput::ordinary("しますか"),
            ConversionOptions::default(),
            Some(CrossCommitBridge {
                tail_reading: "けんとう",
                tail_surface: "検討",
                prefix_right_id: RightContextId::new(0),
                prefix_cost: 0,
            }),
        )
        .expect("bridged conversion");
    assert_eq!(result.candidates()[0].text(), "しますか");
    assert!(result.diagnostics().cross_commit_bridge_spanning_paths > 0);
    assert!(result.diagnostics().cross_commit_bridge_candidates_rescored > 0);
}

#[test]
fn commit_bridge_tail_preserves_the_exact_final_raw_edge() {
    let bytes = synthetic_dictionary(&[
        fixture_entry("まえ", "前", 3, EntryFlags::NONE),
        fixture_entry("あと", "後", 7, EntryFlags::NONE),
    ]);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(&dictionary, "まえあと", ConversionOptions::default())
        .expect("conversion");
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.text() == "前後")
        .expect("composite system candidate");
    assert_eq!(candidate.system_entry_index(), None, "path has two edges");
    let tail = candidate
        .commit_bridge_tail(&dictionary)
        .expect("exact final edge evidence");
    assert_eq!(usize::from(tail.reading_start), "まえ".len());
    assert_eq!(usize::from(tail.text_start), "前".len());
    assert_eq!(tail.prefix_right_id, RightContextId::new(0));
    assert_eq!(tail.prefix_cost, 7);
}

#[test]
fn cross_commit_bridge_fits_128_kib_thread_stack() {
    let bytes = cross_commit_fixture();
    // Production keeps converter arenas in ConversionService's boxed slot
    // pool; the worker stack only borrows a slot for the conversion call.
    // Mirror that ownership so this boundary measures the hot path rather
    // than constructing the reusable arena on the constrained stack.
    let converter = Box::new(Converter::new());
    let handle = std::thread::Builder::new()
        .name("conversion-bridge-128k".to_owned())
        .stack_size(128 * 1024)
        .spawn(move || {
            let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
            let mut converter = converter;
            let result = converter
                .convert_with_user_dictionary_input_bridge_detailed(
                    &dictionary,
                    None,
                    ConversionInput::ordinary("ないか"),
                    ConversionOptions::default(),
                    Some(issue_83_bridge()),
                )
                .expect("128 KiB bridge conversion");
            assert!(result.diagnostics().cross_commit_bridge_attempted);
        })
        .expect("128 KiB bridge thread");
    handle.join().expect("128 KiB bridge conversion thread");
}

#[test]
fn cross_commit_bridge_mismatch_and_budget_exhaustion_fail_closed() {
    let bytes = cross_commit_fixture();
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();
    let baseline = converter
        .convert(&dictionary, "ないか", ConversionOptions::default())
        .expect("baseline conversion")
        .to_vec();

    let mismatched = CrossCommitBridge {
        tail_surface: "洩れ",
        ..issue_83_bridge()
    };
    let result = converter
        .convert_with_user_dictionary_input_bridge_detailed(
            &dictionary,
            None,
            ConversionInput::ordinary("ないか"),
            ConversionOptions::default(),
            Some(mismatched),
        )
        .expect("mismatched bridge conversion");
    assert_eq!(result.candidates(), baseline.as_slice());
    assert_eq!(
        result.diagnostics().cross_commit_bridge_candidates_rescored,
        0
    );

    converter.set_cross_commit_budgets_for_test(0, 0);
    let exhausted = converter
        .convert_with_user_dictionary_input_bridge_detailed(
            &dictionary,
            None,
            ConversionInput::ordinary("ないか"),
            ConversionOptions::default(),
            Some(issue_83_bridge()),
        )
        .expect("budget-exhausted bridge conversion");
    assert_eq!(exhausted.candidates(), baseline.as_slice());
    assert!(exhausted.diagnostics().cross_commit_bridge_attempted);
    assert_eq!(
        exhausted.diagnostics().cross_commit_bridge_terminal,
        Some(super::ConversionSearchTerminal::LatticeBudgetReached)
    );
    assert_eq!(
        exhausted
            .diagnostics()
            .cross_commit_bridge_candidates_rescored,
        0
    );

    converter.set_cross_commit_budgets_for_test(super::MAX_CROSS_COMMIT_LATTICE_NODES, 0);
    let state_exhausted = converter
        .convert_with_user_dictionary_input_bridge_detailed(
            &dictionary,
            None,
            ConversionInput::ordinary("ないか"),
            ConversionOptions::default(),
            Some(issue_83_bridge()),
        )
        .expect("state-budget-exhausted bridge conversion");
    assert_eq!(state_exhausted.candidates(), baseline.as_slice());
    assert_eq!(
        state_exhausted.diagnostics().cross_commit_bridge_terminal,
        Some(super::ConversionSearchTerminal::StateBudgetReached)
    );
    assert_eq!(
        state_exhausted
            .diagnostics()
            .cross_commit_bridge_candidates_rescored,
        0
    );
}

#[test]
fn invalid_or_oversized_cross_commit_evidence_is_not_replayed() {
    let bytes = cross_commit_fixture();
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let tail = "あ".repeat(super::MAX_CROSS_COMMIT_TAIL_BYTES / 3 + 1);
    let surface = "あ".repeat(super::MAX_CROSS_COMMIT_TAIL_SURFACE_BYTES / 3 + 1);
    let current = "あ".repeat(super::MAX_CROSS_COMMIT_CURRENT_BYTES / 3 + 1);
    let mut converter = Converter::new();
    let baseline = converter
        .convert(&dictionary, "ないか", ConversionOptions::default())
        .expect("baseline conversion")
        .to_vec();
    {
        let mut assert_rejected = |bridge: CrossCommitBridge<'_>| {
            let result = converter
                .convert_with_user_dictionary_input_bridge_detailed(
                    &dictionary,
                    None,
                    ConversionInput::ordinary("ないか"),
                    ConversionOptions::default(),
                    Some(bridge),
                )
                .expect("invalid bridge conversion");
            assert_eq!(result.candidates(), baseline.as_slice());
            assert!(!result.diagnostics().cross_commit_bridge_attempted);
        };
        assert_rejected(CrossCommitBridge {
            tail_reading: &tail,
            ..issue_83_bridge()
        });
        assert_rejected(CrossCommitBridge {
            tail_surface: &surface,
            ..issue_83_bridge()
        });
        assert_rejected(CrossCommitBridge {
            tail_reading: "の",
            tail_surface: "の",
            ..issue_83_bridge()
        });
        assert_rejected(CrossCommitBridge {
            prefix_right_id: RightContextId::new(1),
            ..issue_83_bridge()
        });
        assert_rejected(CrossCommitBridge {
            prefix_cost: -1,
            ..issue_83_bridge()
        });
        assert_rejected(CrossCommitBridge {
            prefix_cost: i64::MAX,
            ..issue_83_bridge()
        });
    }

    let oversized_baseline = converter
        .convert(&dictionary, &current, ConversionOptions::default())
        .expect("oversized-current baseline")
        .to_vec();
    let result = converter
        .convert_with_user_dictionary_input_bridge_detailed(
            &dictionary,
            None,
            ConversionInput::ordinary(&current),
            ConversionOptions::default(),
            Some(issue_83_bridge()),
        )
        .expect("oversized-current bridge conversion");
    assert_eq!(result.candidates(), oversized_baseline.as_slice());
    assert!(!result.diagnostics().cross_commit_bridge_attempted);
}

#[test]
fn absent_bridge_and_user_or_exact_candidates_keep_their_authority() {
    let bytes = cross_commit_fixture();
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let user_dictionary = UserDictionary::parse_tsv(
        "# format-version: 1\nreading\tsurface\tpos\tcomment\nないか\t利用者語\tnoun\t\n",
    )
    .expect("user dictionary");
    let mut converter = Converter::new();
    let baseline = converter
        .convert_with_user_dictionary(
            &dictionary,
            Some(&user_dictionary),
            "ないか",
            ConversionOptions::default(),
        )
        .expect("baseline user conversion")
        .to_vec();
    let absent = converter
        .convert_with_user_dictionary_input_bridge_detailed(
            &dictionary,
            Some(&user_dictionary),
            ConversionInput::ordinary("ないか"),
            ConversionOptions::default(),
            None,
        )
        .expect("absent bridge conversion");
    assert_eq!(absent.candidates(), baseline.as_slice());

    let bridged = converter
        .convert_with_user_dictionary_input_bridge_detailed(
            &dictionary,
            Some(&user_dictionary),
            ConversionInput::ordinary("ないか"),
            ConversionOptions::default(),
            Some(issue_83_bridge()),
        )
        .expect("bridged user conversion");
    assert_eq!(bridged.candidates(), baseline.as_slice());
    assert!(!bridged.diagnostics().cross_commit_bridge_attempted);
    let user = bridged
        .candidates()
        .iter()
        .find(|candidate| candidate.text() == "利用者語")
        .expect("user candidate remains reachable");
    assert_eq!(user.path_evidence().user_edges, 1);
    assert_eq!(user.evidence_class(), CandidateEvidence::ExactUser);
    assert!(user.is_trustworthy_whole_reading_exact());
    assert!(!user.was_cross_commit_rescored());

    let system = bridged
        .candidates()
        .iter()
        .find(|candidate| candidate.text() == "内科")
        .expect("system candidate remains reachable");
    assert_eq!(system.evidence_class(), CandidateEvidence::ExactSystem);
    assert!(system.is_trustworthy_whole_reading_exact());

    let exact = converter
        .convert_with_user_dictionary_input_bridge_detailed(
            &dictionary,
            None,
            ConversionInput::new(
                "esp32",
                "ESP32",
                ConversionInputClass::OpaqueAsciiIdentifier,
                LiteralPolicy::ExactTop1,
            ),
            ConversionOptions::default(),
            Some(issue_83_bridge()),
        )
        .expect("exact policy conversion");
    assert_eq!(exact.candidates()[0].text(), "ESP32");
    assert!(exact.candidates()[0].is_synthetic_exact());
    assert!(!exact.diagnostics().cross_commit_bridge_attempted);
}

#[test]
fn classified_literal_inputs_validate_only_their_checked_policy_pair() {
    assert_eq!(ConversionInput::ordinary("かな").validate(), Ok(()));
    assert_eq!(
        ConversionInput::new(
            "esp32",
            "ESP32",
            ConversionInputClass::OpaqueAsciiIdentifier,
            LiteralPolicy::ExactTop1,
        )
        .validate(),
        Ok(())
    );
    assert_eq!(
        ConversionInput::new(
            "rおぐ",
            "rおぐ",
            ConversionInputClass::MixedUnresolvedLatin,
            LiteralPolicy::ExactOnly,
        )
        .validate(),
        Ok(())
    );

    let invalid = [
        ConversionInput::new(
            "esp32",
            "ESP32",
            ConversionInputClass::OpaqueAsciiIdentifier,
            LiteralPolicy::ExactOnly,
        ),
        ConversionInput::new(
            "esp32",
            "ESP-32",
            ConversionInputClass::OpaqueAsciiIdentifier,
            LiteralPolicy::ExactTop1,
        ),
        ConversionInput::new(
            "esp",
            "ESP",
            ConversionInputClass::OpaqueAsciiIdentifier,
            LiteralPolicy::ExactTop1,
        ),
        ConversionInput::new(
            "123",
            "123",
            ConversionInputClass::OpaqueAsciiIdentifier,
            LiteralPolicy::ExactTop1,
        ),
        ConversionInput::new(
            "rおぐ",
            "xおぐ",
            ConversionInputClass::MixedUnresolvedLatin,
            LiteralPolicy::ExactOnly,
        ),
        ConversionInput::new(
            "かな",
            "かな",
            ConversionInputClass::MixedUnresolvedLatin,
            LiteralPolicy::ExactOnly,
        ),
    ];
    assert!(invalid
        .into_iter()
        .all(|input| input.validate() == Err(super::ConversionError::InvalidOptions)));
}

#[test]
fn exact_only_returns_one_literal_and_consumes_one_shot_state() {
    let bytes = synthetic_dictionary(&[
        fixture_entry("こんにちは", "こんにちは", 1, EntryFlags::NONE),
        fixture_entry("きょう", "今日", 1, EntryFlags::NONE),
    ]);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();
    converter.set_civil_date(crate::calendar::CivilDate::from_ymd(2026, 8, 19));
    converter.set_commit_repair_readings(&["こんにちは"]);
    let exact = converter
        .convert_input_detailed(
            &dictionary,
            ConversionInput::new(
                "rおぐ",
                "rおぐ",
                ConversionInputClass::MixedUnresolvedLatin,
                LiteralPolicy::ExactOnly,
            ),
            ConversionOptions::default(),
        )
        .expect("exact-only conversion");
    assert_eq!(exact.candidates().len(), 1);
    assert_eq!(exact.candidates()[0].text(), "rおぐ");
    assert!(exact.candidates()[0].is_synthetic_exact());
    assert_eq!(
        exact.candidates()[0].evidence_class(),
        CandidateEvidence::Fallback
    );

    let later = converter
        .convert(&dictionary, "あいう", ConversionOptions::default())
        .expect("ordinary conversion after exact-only");
    assert!(!later
        .iter()
        .any(|candidate| candidate.text() == "こんにちは"));
    let calendar = converter
        .convert(&dictionary, "きょう", ConversionOptions::default())
        .expect("ordinary conversion after one-shot date");
    assert!(calendar
        .iter()
        .all(|candidate| !candidate.path_evidence().generated_edges.gt(&0)));
}

#[test]
fn whole_reading_lexical_number_form_stays_ahead_of_generated_duplicates() {
    let bytes = synthetic_dictionary(&[fixture_entry("いちにち", "一日", 2_000, EntryFlags::NONE)]);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(&dictionary, "いちにち", ConversionOptions::default())
        .expect("numeric lexical conversion");
    let surfaces: Vec<&str> = candidates
        .iter()
        .map(|candidate| candidate.text())
        .collect();
    let ranking: Vec<(&str, i64, Option<u32>, super::PathEvidence)> = candidates
        .iter()
        .map(|candidate| {
            (
                candidate.text(),
                candidate.cost,
                candidate.system_entry_index(),
                candidate.path_evidence(),
            )
        })
        .collect();

    assert_eq!(
        surfaces.first().copied(),
        Some("一日"),
        "unexpected numeric order: {ranking:?}"
    );
    assert_eq!(
        surfaces
            .iter()
            .filter(|surface| **surface == "一日")
            .count(),
        1
    );
    assert_eq!(
        surfaces.iter().filter(|surface| **surface == "1日").count(),
        1
    );
    let generated = candidates
        .iter()
        .find(|candidate| candidate.text() == "1日")
        .expect("generated numeric candidate");
    assert_eq!(generated.evidence_class(), CandidateEvidence::Generated);
    assert!(!generated.is_trustworthy_whole_reading_exact());
}

#[test]
fn generated_day_suffixes_stay_behind_lexical_verb_phrases() {
    let bytes = synthetic_dictionary(&[
        // The split lexical paths model the verb stem followed by the
        // question ending. Their combined cost is below the guarded
        // generated day suffix, but above the old unguarded form.
        fixture_entry("つづけ", "続け", 1_000, EntryFlags::NONE),
        fixture_entry("すすめ", "進め", 1_000, EntryFlags::NONE),
        fixture_entry("よう", "よう", 1_000, EntryFlags::NONE),
        fixture_entry("か", "か", 1_000, EntryFlags::NONE),
    ]);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();

    let standalone = converter
        .convert(&dictionary, "ようか", ConversionOptions::default())
        .expect("standalone calendar day");
    assert!(
        standalone
            .iter()
            .any(|candidate| candidate.text() == "八日"),
        "standalone ようか must retain the traditional day form: {standalone:?}"
    );

    for (reading, expected) in [
        ("つづけようか", "続けようか"),
        ("すすめようか", "進めようか"),
    ] {
        let candidates = converter
            .convert(&dictionary, reading, ConversionOptions::default())
            .expect("lexical verb phrase");
        assert_eq!(
            candidates.first().map(ConversionCandidate::text),
            Some(expected),
            "generated ようか must not overtake the lexical phrase for {reading}: {candidates:?}"
        );
        let lexical_cost = candidates
            .iter()
            .find(|candidate| candidate.text() == expected)
            .map(|candidate| candidate.cost)
            .expect("lexical phrase candidate");
        assert!(
                candidates
                    .iter()
                    .filter(|candidate| candidate.generated_day_suffix)
                    .all(|candidate| candidate.cost > lexical_cost),
                "the generated day splice must be demoted below lexical evidence for {reading}: {candidates:?}"
            );

        // The admission price must protect the top result even when the
        // caller asks for only one candidate, before an N-best lexical
        // alternative could be retained for the relative demotion pass.
        let top_one = converter
            .convert(
                &dictionary,
                reading,
                ConversionOptions {
                    max_candidates: 1,
                    ..ConversionOptions::default()
                },
            )
            .expect("single-candidate lexical verb phrase");
        assert_eq!(top_one[0].text(), expected, "single-candidate {reading}");
    }
}

#[test]
fn generated_day_suffix_remains_available_without_a_lexical_whole_path() {
    let bytes = synthetic_dictionary(&[fixture_entry("ことし", "今年", 1_000, EntryFlags::NONE)]);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(&dictionary, "ことしようか", ConversionOptions::default())
        .expect("calendar compound");
    assert!(
            candidates.iter().any(|candidate| {
                candidate.text().starts_with("今年")
                    && candidate.path_evidence().generated_edges > 0
            }),
            "a legal numeric compound must remain available without a lexical whole path: {candidates:?}"
        );
}

#[test]
fn bos_filters_non_initial_fragments_but_compound_paths_can_use_them() {
    let bytes = synthetic_dictionary(&[
        fixture_entry("ずかい", "図解", 1_000, EntryFlags::NONE),
        fixture_entry("ずかい", "使い", 100, EntryFlags::NON_INITIAL),
        fixture_entry("つかい", "使い", 100, EntryFlags::NONE),
        fixture_entry("き", "気", 100, EntryFlags::NONE),
        fixture_entry("づかい", "遣い", 100, EntryFlags::NON_INITIAL),
    ]);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();

    let independent = converter
        .convert(&dictionary, "ずかい", ConversionOptions::default())
        .expect("independent conversion");
    assert!(independent
        .iter()
        .any(|candidate| candidate.text() == "図解"));
    assert!(independent
        .iter()
        .all(|candidate| candidate.text() != "使い"));

    let ordinary = converter
        .convert(&dictionary, "つかい", ConversionOptions::default())
        .expect("ordinary unvoiced conversion");
    assert!(ordinary.iter().any(|candidate| candidate.text() == "使い"));

    let compound = converter
        .convert(&dictionary, "きづかい", ConversionOptions::default())
        .expect("compound conversion");
    assert!(
        compound
            .iter()
            .any(|candidate| candidate.text() == "気遣い"),
        "non-initial fragment was lost inside a compound: {:?}",
        compound
            .iter()
            .map(|candidate| candidate.text())
            .collect::<Vec<_>>()
    );
}

#[test]
fn trustworthy_exact_word_suppresses_unconfirmed_repairs_and_costly_mosaics() {
    let bytes = synthetic_dictionary(&[
        fixture_entry("ずかい", "図解", 1_000, EntryFlags::NONE),
        fixture_entry("ずがい", "頭蓋", 100, EntryFlags::NONE),
        fixture_entry("ず", "図", 3_000, EntryFlags::NONE),
        fixture_entry("か", "書", 3_000, EntryFlags::NONE),
        fixture_entry("い", "い", 3_000, EntryFlags::NONE),
    ]);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(&dictionary, "ずかい", ConversionOptions::default())
        .expect("exact conversion");
    let ranking = candidates
        .iter()
        .map(|candidate| (candidate.text(), candidate.cost, candidate.path_evidence()))
        .collect::<Vec<_>>();

    assert!(candidates
        .iter()
        .any(|candidate| candidate.text() == "図解"));
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.text() != "頭蓋"),
        "dakuten repair polluted an exact query: {ranking:?}"
    );
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.text() != "図書い"),
        "costly split mosaic survived an exact word: {ranking:?}"
    );
}

#[test]
fn repair_candidate_remains_available_when_no_trustworthy_exact_word_exists() {
    let bytes = synthetic_dictionary(&[fixture_entry("ずがい", "頭蓋", 100, EntryFlags::NONE)]);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(&dictionary, "ずかい", ConversionOptions::default())
        .expect("repair-only conversion");
    let repaired = candidates
        .iter()
        .find(|candidate| candidate.text() == "頭蓋")
        .expect("dakuten repair remains available");
    assert!(repaired.path_evidence().has_repair_kind(RepairKind::Rule));
    assert!(repaired.path_evidence().has_unconfirmed_repair());
    assert_eq!(
        repaired.evidence_class(),
        CandidateEvidence::Repair(RepairKind::Rule)
    );
    assert!(!repaired.is_trustworthy_whole_reading_exact());
}

#[test]
fn english_spelling_repair_is_not_promoted_to_exact_evidence() {
    let bytes =
        synthetic_dictionary(&[fixture_entry("アップル", "アップル", 100, EntryFlags::NONE)]);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(&dictionary, "あｐｐｌｅ", ConversionOptions::default())
        .expect("English spelling conversion");
    let repaired = candidates
        .iter()
        .find(|candidate| candidate.text() == "アップル")
        .expect("English spelling repair remains available");
    assert_eq!(
        repaired.evidence_class(),
        CandidateEvidence::Repair(RepairKind::EnglishSpelling)
    );
    assert!(!repaired.is_trustworthy_whole_reading_exact());
}

#[test]
fn exact_top1_is_fixed_zero_and_admits_only_full_span_non_spelling_edges() {
    let bytes = synthetic_dictionary(&[
        fixture_entry("esp32", "SystemExact", 1, EntryFlags::NONE),
        fixture_entry("esp32", "SpellingExact", 0, EntryFlags::SPELLING_CORRECTION),
        fixture_entry("esp", "Partial", 0, EntryFlags::NONE),
        fixture_entry("2", "GeneratedLike", 0, EntryFlags::NONE),
    ]);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let user_dictionary =
        UserDictionary::parse_tsv("reading\tsurface\tpos\tcomment\nesp32\tUserExact\talphabet\t\n")
            .expect("alphabet user dictionary");
    let input = ConversionInput::new(
        "esp32",
        "ESP32",
        ConversionInputClass::OpaqueAsciiIdentifier,
        LiteralPolicy::ExactTop1,
    );
    let mut converter = Converter::new();
    let result = converter
        .convert_with_user_dictionary_input_detailed(
            &dictionary,
            Some(&user_dictionary),
            input,
            ConversionOptions {
                max_candidates: 4,
                ..ConversionOptions::default()
            },
        )
        .expect("exact-top1 conversion");
    let candidates = result.candidates();
    assert!(!candidates.is_empty());
    assert_eq!(candidates[0].text(), "ESP32");
    assert!(candidates[0].is_synthetic_exact());
    assert!(candidates.len() <= 4);
    assert!(candidates
        .iter()
        .skip(1)
        .any(|candidate| candidate.text() == "SystemExact"));
    assert!(candidates
        .iter()
        .skip(1)
        .any(|candidate| candidate.text() == "UserExact"));
    assert!(!candidates
        .iter()
        .any(|candidate| candidate.text() == "SpellingExact"));
    assert!(!candidates
        .iter()
        .any(|candidate| candidate.text() == "Partial"));
    assert!(!candidates
        .iter()
        .any(|candidate| candidate.text() == "GeneratedLike"));
    assert!(candidates.iter().skip(1).all(|candidate| {
        candidate.segments().len() == 1
            && candidate.segments()[0].reading_start == 0
            && candidate.segments()[0].reading_end == "esp32".len() as u16
    }));
}

#[test]
fn raw_multi_pass_preserves_direct_order_and_admits_only_system_paths() {
    let bytes = synthetic_dictionary(&[
        fixture_entry("あき", "SAME", 100, EntryFlags::NONE),
        fixture_entry("あき", "DIRECT", 200, EntryFlags::NONE),
        fixture_entry("あか", "SAME", 1, EntryFlags::NONE),
        fixture_entry("あか", "CHEAP", 2, EntryFlags::NONE),
        fixture_entry("あ", "A", 10, EntryFlags::NONE),
        fixture_entry("か", "C", 10, EntryFlags::NONE),
    ]);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let options = ConversionOptions {
        max_candidates: 9,
        raw_repair_budget: RawRepairBudget {
            max_corrected_passes: 2,
            max_repair_candidates: 4,
            max_lattice_nodes: 64,
            max_search_states: 128,
        },
        ..ConversionOptions::default()
    };
    let mut converter = Converter::new();
    let direct = converter
        .convert(&dictionary, "あき", options)
        .expect("direct conversion")
        .iter()
        .map(|candidate| candidate.text().to_owned())
        .collect::<Vec<_>>();
    let direct_count = direct.len();
    assert!(direct_count < options.max_candidates);

    let plan = local_plan(7, "あき", "あか", RepairTier::LocalCompletion);
    let result = converter
        .convert_with_raw_repair_plans(&dictionary, None, "あき", &[plan], options)
        .expect("one-slot conversion");
    let candidates = result.candidates();
    let diagnostics = result.diagnostics();
    assert_eq!(diagnostics.raw_repair_passes, 1);
    assert!(diagnostics.raw_repair_lattice_nodes > 0);
    assert!(diagnostics.raw_repair_search_states > 0);
    assert_eq!(
        candidates[..direct_count]
            .iter()
            .map(|candidate| candidate.text().to_owned())
            .collect::<Vec<_>>(),
        direct
    );
    assert!(candidates[..direct_count]
        .iter()
        .all(|candidate| candidate.origin() == CandidateOrigin::Direct));
    let same = candidates
        .iter()
        .find(|candidate| candidate.text() == "SAME")
        .expect("direct same-surface candidate");
    assert_eq!(same.origin(), CandidateOrigin::Direct);
    let cheap_index = candidates
        .iter()
        .position(|candidate| candidate.text() == "CHEAP")
        .expect("cheaper repaired candidate");
    assert!(cheap_index >= direct_count);
    assert_eq!(
        candidates[cheap_index].origin(),
        CandidateOrigin::RawRepair {
            plan_id: 7,
            tier: RepairTier::LocalCompletion,
        }
    );
    let compound = candidates
        .iter()
        .find(|candidate| candidate.text() == "AC")
        .expect("corrected multi-segment system candidate");
    assert_eq!(compound.segments().len(), 2);
    assert_eq!(compound.path_evidence().system_edges, 2);
    assert!(compound.has_full_system_coverage("あか".len()));
    assert!(candidates.iter().all(|candidate| {
        candidate.origin() == CandidateOrigin::Direct || candidate.path_evidence().is_system_only()
    }));
}

#[test]
fn raw_multi_pass_preserves_mixed_exact_direct_before_admitted_repair() {
    let bytes = synthetic_dictionary(&[
        fixture_entry("ないk", "HOSTILE", 0, EntryFlags::NONE),
        fixture_entry("ないか", "内科", 1, EntryFlags::NONE),
    ]);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let input = ConversionInput::new(
        "ないk",
        "ないk",
        ConversionInputClass::MixedUnresolvedLatin,
        LiteralPolicy::ExactOnly,
    );
    let plan = local_plan(8, "ないk", "ないか", RepairTier::LocalCompletion);
    let mut converter = Converter::new();
    let result = converter
        .convert_input_with_raw_repair_plans(
            &dictionary,
            None,
            input,
            &[plan],
            ConversionOptions {
                max_candidates: 9,
                ..ConversionOptions::default()
            },
        )
        .expect("classified one-slot conversion");
    let candidates = result.candidates();
    let direct_count = candidates
        .iter()
        .take_while(|candidate| candidate.origin() == CandidateOrigin::Direct)
        .count();
    assert_eq!(direct_count, 1);
    assert_eq!(candidates[0].text(), "ないk");
    assert!(candidates[0].is_synthetic_exact());
    assert!(!candidates
        .iter()
        .any(|candidate| candidate.text() == "HOSTILE"));
    let repaired = candidates
        .iter()
        .find(|candidate| candidate.text() == "内科")
        .expect("system-only corrected candidate");
    assert_eq!(
        repaired.origin(),
        CandidateOrigin::RawRepair {
            plan_id: 8,
            tier: RepairTier::LocalCompletion,
        }
    );
    assert_eq!(result.diagnostics().raw_repair_passes, 1);
    assert_eq!(result.diagnostics().raw_repair_candidates_added, 1);

    let mut exact_only_converter = Converter::new();
    let exact_only = exact_only_converter
        .convert_input_with_raw_repair_plans(
            &dictionary,
            None,
            input,
            &[local_plan(
                9,
                "ないk",
                "ないか",
                RepairTier::LocalCompletion,
            )],
            ConversionOptions {
                max_candidates: 1,
                ..ConversionOptions::default()
            },
        )
        .expect("full exact-only result");
    assert_eq!(exact_only.candidates().len(), 1);
    assert_eq!(exact_only.candidates()[0].text(), "ないk");
    assert!(exact_only.candidates()[0].is_synthetic_exact());
    assert_eq!(exact_only.diagnostics().raw_repair_passes, 0);
    assert_eq!(exact_only.diagnostics().raw_repair_candidates_added, 0);
}

#[test]
fn raw_multi_pass_aggregate_budgets_bound_sequential_passes() {
    let bytes = synthetic_dictionary(&[
        fixture_entry("あき", "DIRECT", 1, EntryFlags::NONE),
        fixture_entry("あか", "FIRST", 1, EntryFlags::NONE),
        fixture_entry("あく", "SECOND", 1, EntryFlags::NONE),
    ]);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let plans = [
        local_plan(10, "あき", "あか", RepairTier::LocalCompletion),
        local_plan(11, "あき", "あく", RepairTier::LocalCompletion),
    ];
    let options = ConversionOptions {
        max_candidates: 9,
        raw_repair_budget: RawRepairBudget {
            max_corrected_passes: 1,
            max_repair_candidates: 1,
            max_lattice_nodes: 64,
            max_search_states: 128,
        },
        ..ConversionOptions::default()
    };
    let mut converter = Converter::new();
    let result = converter
        .convert_with_raw_repair_plans(&dictionary, None, "あき", &plans, options)
        .expect("bounded multi-pass conversion");
    let diagnostics = result.diagnostics();
    assert_eq!(diagnostics.raw_repair_passes, 1);
    assert!(diagnostics.raw_repair_candidates_added <= 1);
    assert!(diagnostics.raw_repair_candidates_examined <= 1);
    assert!(diagnostics.raw_repair_lattice_nodes <= 64);
    assert!(diagnostics.raw_repair_search_states <= 128);
    assert!(!result
        .candidates()
        .iter()
        .any(|candidate| candidate.text() == "SECOND"));

    let lattice_limited = {
        let mut converter = Converter::new();
        let result = converter
            .convert_with_raw_repair_plans(
                &dictionary,
                None,
                "あき",
                &plans,
                ConversionOptions {
                    max_candidates: 9,
                    raw_repair_budget: RawRepairBudget {
                        max_corrected_passes: 4,
                        max_repair_candidates: 9,
                        max_lattice_nodes: 1,
                        max_search_states: 128,
                    },
                    ..ConversionOptions::default()
                },
            )
            .expect("lattice aggregate limit");
        result.diagnostics()
    };
    assert_eq!(lattice_limited.raw_repair_passes, 1);
    assert_eq!(lattice_limited.raw_repair_lattice_nodes, 1);

    let search_limited = {
        let mut converter = Converter::new();
        let result = converter
            .convert_with_raw_repair_plans(
                &dictionary,
                None,
                "あき",
                &plans,
                ConversionOptions {
                    max_candidates: 9,
                    raw_repair_budget: RawRepairBudget {
                        max_corrected_passes: 4,
                        max_repair_candidates: 9,
                        max_lattice_nodes: 64,
                        max_search_states: 1,
                    },
                    ..ConversionOptions::default()
                },
            )
            .expect("search aggregate limit");
        result.diagnostics()
    };
    assert_eq!(search_limited.raw_repair_passes, 1);
    assert_eq!(search_limited.raw_repair_search_states, 1);
}

#[test]
fn raw_multi_pass_core_path_fits_128_kib_thread_stack() {
    let bytes = synthetic_dictionary(&[
        fixture_entry("あき", "DIRECT", 1, EntryFlags::NONE),
        fixture_entry("あか", "CORRECTED", 1, EntryFlags::NONE),
    ]);
    // ConversionService owns each Converter in a boxed reusable slot. Do
    // not charge construction of that process-lifetime arena to the worker
    // stack whose conversion call this test is intended to bound.
    let converter = Box::new(Converter::new());
    let handle = std::thread::Builder::new()
        .name("conversion-raw-128k".to_owned())
        .stack_size(128 * 1024)
        .spawn(move || {
            let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
            let plan = local_plan(12, "あき", "あか", RepairTier::LocalCompletion);
            let mut converter = converter;
            let result = converter
                .convert_with_raw_repair_plans(
                    &dictionary,
                    None,
                    "あき",
                    &[plan],
                    ConversionOptions::default(),
                )
                .expect("128 KiB conversion");
            assert!(result.diagnostics().raw_repair_passes <= 1);
        })
        .expect("128 KiB thread");
    handle.join().expect("128 KiB conversion thread");
}

#[test]
fn raw_multi_pass_skips_general_tier_and_keeps_full_direct_without_repair() {
    let bytes = synthetic_dictionary(&[
        fixture_entry("あき", "ONE", 1, EntryFlags::NONE),
        fixture_entry("あき", "TWO", 2, EntryFlags::NONE),
    ]);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let plan = local_plan(1, "あき", "あか", RepairTier::GeneralSingleInsertion);
    let mut converter = Converter::new();
    let options = ConversionOptions {
        max_candidates: 4,
        ..ConversionOptions::default()
    };
    let general = converter
        .convert_with_raw_repair_plans(&dictionary, None, "あき", &[plan], options)
        .expect("general tier is a safe rejection");
    assert_eq!(general.diagnostics().raw_repair_passes, 0);
    assert_eq!(general.diagnostics().raw_repair_candidates_added, 0);
    assert!(general
        .candidates()
        .iter()
        .all(|candidate| candidate.origin() == CandidateOrigin::Direct));

    let full_options = ConversionOptions {
        max_candidates: 3,
        ..ConversionOptions::default()
    };
    let full = converter
        .convert_with_raw_repair_plans(
            &dictionary,
            None,
            "あき",
            &[local_plan(2, "あき", "あか", RepairTier::LocalCompletion)],
            full_options,
        )
        .expect("direct full result");
    assert_eq!(full.diagnostics().raw_repair_passes, 1);
    assert_eq!(full.diagnostics().raw_repair_candidates_added, 0);
    assert_eq!(
        full.candidates()
            .iter()
            .map(|candidate| candidate.text())
            .collect::<Vec<_>>(),
        vec!["ONE", "TWO", "あき"]
    );
}

#[test]
fn raw_multi_pass_full_direct_reserves_one_tail_slot_for_best_local_repair() {
    let mut rows = (0..18)
        .map(|index| {
            fixture_entry(
                "あき",
                &format!("DIRECT-{index:02}"),
                index,
                EntryFlags::NONE,
            )
        })
        .collect::<Vec<_>>();
    for index in 0..4 {
        rows.push(fixture_entry(
            "あ",
            &format!("PREFIX-{index}"),
            100 + index,
            EntryFlags::NONE,
        ));
        rows.push(fixture_entry(
            "き",
            &format!("SUFFIX-{index}"),
            100 + index,
            EntryFlags::NONE,
        ));
    }
    rows.push(fixture_entry(
        "なずか",
        "OTHER-REPAIR",
        100,
        EntryFlags::NONE,
    ));
    rows.push(fixture_entry("なぜか", "内科", -100, EntryFlags::NONE));
    let bytes = synthetic_dictionary(&rows);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let plans = [
        local_plan(20, "あき", "なずか", RepairTier::LocalCompletion),
        local_plan(21, "あき", "なぜか", RepairTier::LocalCompletion),
    ];
    let options = ConversionOptions {
        max_candidates: 18,
        ..ConversionOptions::default()
    };
    let mut converter = Converter::new();
    let direct = converter
        .convert(&dictionary, "あき", options)
        .expect("full direct baseline")
        .iter()
        .map(|candidate| candidate.text().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(direct.len(), 18);
    let no_repair = converter
        .convert_with_raw_repair_plans(
            &dictionary,
            None,
            "あき",
            &[local_plan(
                19,
                "あき",
                "なぬか",
                RepairTier::LocalCompletion,
            )],
            options,
        )
        .expect("full direct result without an admissible repair");
    assert_eq!(
        no_repair
            .candidates()
            .iter()
            .map(|candidate| candidate.text().to_owned())
            .collect::<Vec<_>>(),
        direct
    );
    assert_eq!(no_repair.diagnostics().raw_repair_candidates_added, 0);
    let result = converter
        .convert_with_raw_repair_plans(&dictionary, None, "あき", &plans, options)
        .expect("full direct result with one repair reservation");
    let candidates = result.candidates();
    assert_eq!(candidates.len(), 18);
    assert_eq!(candidates[0].text(), direct[0]);
    assert_eq!(candidates[0].origin(), CandidateOrigin::Direct);
    assert_eq!(
        candidates[..17]
            .iter()
            .map(|candidate| candidate.text().to_owned())
            .collect::<Vec<_>>(),
        direct[..17]
    );
    assert!(!candidates
        .iter()
        .any(|candidate| candidate.text() == direct[17]));
    assert!(!candidates
        .iter()
        .any(|candidate| candidate.text() == "OTHER-REPAIR"));
    assert_eq!(candidates[17].text(), "内科");
    assert_eq!(
        candidates[17].origin(),
        CandidateOrigin::RawRepair {
            plan_id: 21,
            tier: RepairTier::LocalCompletion,
        }
    );
    assert_eq!(
        candidates[17].evidence_class(),
        CandidateEvidence::RawRepair {
            tier: RepairTier::LocalCompletion,
        }
    );
    assert_eq!(result.diagnostics().raw_repair_passes, 2);
    assert_eq!(result.diagnostics().raw_repair_candidates_added, 1);
}

#[test]
fn raw_multi_pass_rejects_user_fallback_generated_and_spelling_paths() {
    let bytes = synthetic_dictionary(&[
        fixture_entry("あき", "DIRECT", 1, EntryFlags::NONE),
        fixture_entry("あか", "SPELL", 1, EntryFlags::SPELLING_CORRECTION),
    ]);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let user_dictionary = UserDictionary::parse_tsv(
        "# format-version: 1\nreading\tsurface\tpos\tcomment\nあか\tUSER\tnoun\t\n",
    )
    .expect("user dictionary");
    let options = ConversionOptions {
        max_candidates: 5,
        ..ConversionOptions::default()
    };
    let mut converter = Converter::new();
    let result = converter
        .convert_with_raw_repair_plans(
            &dictionary,
            Some(&user_dictionary),
            "あき",
            &[local_plan(3, "あき", "あか", RepairTier::LocalCompletion)],
            options,
        )
        .expect("repair rejection keeps direct result");
    assert_eq!(result.diagnostics().raw_repair_candidates_added, 0);
    assert!(result
        .candidates()
        .iter()
        .all(|candidate| candidate.origin() == CandidateOrigin::Direct));
    assert!(!result
        .candidates()
        .iter()
        .any(|candidate| candidate.text() == "USER"));
    assert!(!result
        .candidates()
        .iter()
        .any(|candidate| candidate.text() == "SPELL"));

    let fallback = converter
        .convert_with_raw_repair_plans(
            &dictionary,
            None,
            "あき",
            &[local_plan(4, "あき", "みす", RepairTier::LocalCompletion)],
            options,
        )
        .expect("fallback rejection keeps direct result");
    assert_eq!(fallback.diagnostics().raw_repair_candidates_added, 0);

    let generated = converter
        .convert_with_raw_repair_plans(
            &dictionary,
            None,
            "あき",
            &[local_plan(5, "あき", "2", RepairTier::LocalCompletion)],
            options,
        )
        .expect("generated rejection keeps direct result");
    assert_eq!(generated.diagnostics().raw_repair_candidates_added, 0);
}

#[test]
fn raw_multi_pass_corrected_pass_skips_input_repair_and_consumes_one_shot_state() {
    let bytes = synthetic_dictionary(&[
        fixture_entry("x", "ORIGINAL", 1, EntryFlags::NONE),
        fixture_entry("こんにちは", "RULE_TARGET", 1, EntryFlags::NONE),
        fixture_entry("きょう", "今日", 1, EntryFlags::NONE),
    ]);
    let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
    let mut converter = Converter::new();
    converter.set_civil_date(crate::calendar::CivilDate::from_ymd(2026, 8, 19));
    let options = ConversionOptions {
        max_candidates: 5,
        ..ConversionOptions::default()
    };
    let result = converter
        .convert_with_raw_repair_plans(
            &dictionary,
            None,
            "x",
            &[local_plan(6, "x", "こにちは", RepairTier::LocalCompletion)],
            options,
        )
        .expect("skip-input-repair corrected pass");
    assert_eq!(result.diagnostics().raw_repair_passes, 1);
    assert_eq!(result.diagnostics().raw_repair_candidates_added, 0);
    assert!(!result
        .candidates()
        .iter()
        .any(|candidate| candidate.text() == "RULE_TARGET"));

    // The one-shot date was consumed by the direct pass; a subsequent
    // direct request must not inherit date surfaces from this request.
    let later = converter
        .convert(&dictionary, "きょう", options)
        .expect("later conversion");
    assert!(later
        .iter()
        .all(|candidate| !candidate.text().contains("2026年")));

    let mut invalid_request = Converter::new();
    invalid_request.set_civil_date(crate::calendar::CivilDate::from_ymd(2026, 8, 19));
    assert!(invalid_request
        .convert(
            &dictionary,
            "きょう",
            ConversionOptions {
                max_candidates: 0,
                ..options
            },
        )
        .is_err());
    let after_invalid = invalid_request
        .convert(&dictionary, "きょう", options)
        .expect("conversion after rejected request");
    assert!(after_invalid
        .iter()
        .all(|candidate| !candidate.text().contains("2026年")));
}
