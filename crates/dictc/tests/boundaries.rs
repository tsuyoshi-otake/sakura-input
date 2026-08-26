//! The optional bunsetsu-boundary table: compile-side encoding, reader
//! validation, and the conversion-segment fusion it exists for.
//!
//! The fixture mirrors the real defect this table fixes: the cheapest lattice
//! path for した is 動詞「し」+ 助動詞「た」, which used to become two
//! one-morpheme segments and made the whole-reading candidate 下 unreachable
//! from the candidate window.

use dictc::segmenter::{build_boundaries, parse_mozc_pos_features, parse_mozc_segmenter_rules};
use dictc::{compile, compile_with_tables, parse_connection, parse_entries, OptionalTables};
use sakura_core::conversion::{ConversionOptions, Converter};
use sakura_core::dictionary::{Dictionary, EntryFlags};

const ENTRIES: &str = "# license: BSD-3-Clause\n\
reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
し\t試\t1\t1\t100\t-\tit\t\n\
た\tた\t2\t2\t100\t-\tpredict\t\n\
した\t下\t3\t3\t3000\t-\t\t\n";

const CONNECTION: &str = "# license: BSD-3-Clause\n\
classes\t4\n\
default\t0\n";

const ID_DEF: &str = "0 BOS/EOS,*,*,*,*,*,*\n\
1 動詞,自立,*,*,サ変・スル,連用形,する\n\
2 助動詞,特殊・タ,基本形,*,*,*,た\n\
3 名詞,一般,*,*,*,*,*\n";

const SEGMENTER: &str = "# an ancillary word continues the current bunsetsu\n\
* ^(助詞|助動詞) false\n\
* * true\n";

fn fixture_boundaries() -> dictc::segmenter::BunsetsuBoundaries {
    let features = parse_mozc_pos_features("id.def", ID_DEF).expect("features");
    let rules = parse_mozc_segmenter_rules("segmenter.def", SEGMENTER).expect("rules");
    build_boundaries("segmenter.def", &features, &rules).expect("boundaries")
}

fn image_with_boundaries() -> Vec<u8> {
    let entries = parse_entries("fixture.tsv", ENTRIES).expect("entries");
    let connection = parse_connection("connection.tsv", CONNECTION, false).expect("matrix");
    compile_with_tables(
        &entries,
        &connection,
        OptionalTables {
            boundaries: Some(&fixture_boundaries()),
            ..OptionalTables::default()
        },
    )
    .expect("compile with boundaries")
}

fn image_without_boundaries() -> Vec<u8> {
    let entries = parse_entries("fixture.tsv", ENTRIES).expect("entries");
    let connection = parse_connection("connection.tsv", CONNECTION, false).expect("matrix");
    compile(&entries, &connection).expect("compile without boundaries")
}

#[test]
fn boundary_table_round_trips_and_stays_deterministic() {
    let first = image_with_boundaries();
    assert_eq!(first, image_with_boundaries());

    let dictionary = Dictionary::parse(&first).expect("parse");
    assert!(dictionary.has_bunsetsu_boundaries());
    // 動詞|助動詞 fuses; everything else in the fixture is a boundary.
    assert_eq!(dictionary.bunsetsu_boundary(1, 2), Some(false));
    assert_eq!(dictionary.bunsetsu_boundary(2, 1), Some(true));
    assert_eq!(dictionary.bunsetsu_boundary(1, 3), Some(true));
    for class in 0..4u16 {
        assert_eq!(dictionary.bunsetsu_boundary(0, class), Some(true));
        assert_eq!(dictionary.bunsetsu_boundary(class, 0), Some(true));
    }
    // Out-of-range classes fail closed to a boundary instead of fusing.
    assert_eq!(dictionary.bunsetsu_boundary(99, 1), Some(true));

    let legacy = image_without_boundaries();
    let legacy = Dictionary::parse(&legacy).expect("parse legacy");
    assert!(!legacy.has_bunsetsu_boundaries());
    assert_eq!(legacy.bunsetsu_boundary(1, 2), None);
}

#[test]
fn conversion_fuses_morphemes_into_bunsetsu_segments() {
    let bytes = image_with_boundaries();
    let dictionary = Dictionary::parse(&bytes).expect("parse");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(&dictionary, "した", ConversionOptions::default())
        .expect("convert");

    // The cheapest path is still 試+た, but it now presents as one segment
    // covering the whole reading, so the candidate window enumerates every
    // whole-reading alternative (下) instead of hiding it behind 「し」.
    let best = &candidates[0];
    assert_eq!(best.text(), "試た");
    assert_eq!(best.segments().len(), 1);
    let segment = best.segments()[0];
    assert_eq!(
        (segment.reading_start, segment.reading_end),
        (0, u16::try_from("した".len()).expect("fits"))
    );
    assert_eq!(
        (segment.text_start, segment.text_end),
        (0, u16::try_from("試た".len()).expect("fits"))
    );
    assert_eq!((segment.left_id, segment.right_id), (1, 2));
    assert!(segment.flags.contains(EntryFlags::IT));
    assert!(segment.flags.contains(EntryFlags::PREDICTION));
    // The OR-merged flags above must not let the non-IT 助動詞 count as an
    // IT word: the fused segment still knows it covers two words, one IT.
    assert_eq!(segment.word_count, 2);
    assert_eq!(segment.it_word_count, 1);

    let whole_word = candidates
        .iter()
        .find(|candidate| candidate.text() == "下")
        .expect("whole-reading candidate is listed");
    assert_eq!(whole_word.segments().len(), 1);
    assert_eq!(whole_word.segments()[0].word_count, 1);
    assert_eq!(whole_word.segments()[0].it_word_count, 0);
}

#[test]
fn images_without_the_table_keep_morpheme_segments() {
    let bytes = image_without_boundaries();
    let dictionary = Dictionary::parse(&bytes).expect("parse");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(&dictionary, "した", ConversionOptions::default())
        .expect("convert");
    assert_eq!(candidates[0].text(), "試た");
    assert_eq!(candidates[0].segments().len(), 2);
    for segment in candidates[0].segments() {
        assert_eq!(segment.word_count, 1);
    }
    assert_eq!(candidates[0].segments()[0].it_word_count, 1);
    assert_eq!(candidates[0].segments()[1].it_word_count, 0);
}

#[test]
fn compile_rejects_a_class_count_mismatch() {
    let entries = parse_entries("fixture.tsv", ENTRIES).expect("entries");
    let wider = parse_connection(
        "connection.tsv",
        "# license: BSD-3-Clause\nclasses\t8\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    assert!(compile_with_tables(
        &entries,
        &wider,
        OptionalTables {
            boundaries: Some(&fixture_boundaries()),
            ..OptionalTables::default()
        },
    )
    .is_err());
}

#[test]
fn corrupted_boundary_tables_are_rejected_not_trusted() {
    let bytes = image_with_boundaries();
    let table_at = bytes
        .windows(4)
        .position(|window| window == b"SBD1")
        .expect("boundary table present");

    // Header magic.
    let mut corrupt = bytes.clone();
    corrupt[table_at] ^= 0xff;
    assert!(Dictionary::parse(&corrupt).is_err());

    // Declared class count disagrees with the image header.
    let mut corrupt = bytes.clone();
    corrupt[table_at + 4] ^= 0x01;
    assert!(Dictionary::parse(&corrupt).is_err());

    // Reserved header bytes must stay zero.
    let mut corrupt = bytes.clone();
    corrupt[table_at + 6] = 1;
    assert!(Dictionary::parse(&corrupt).is_err());

    // A set padding bit past the 4 fixture classes (row stride is one byte).
    let mut corrupt = bytes.clone();
    corrupt[table_at + 8] |= 1 << 5;
    assert!(Dictionary::parse(&corrupt).is_err());

    // BOS/EOS must stay a boundary on both axes: clear (0, 1) then (2, 0).
    let mut corrupt = bytes.clone();
    corrupt[table_at + 8] &= !(1 << 1);
    assert!(Dictionary::parse(&corrupt).is_err());
    let mut corrupt = bytes.clone();
    corrupt[table_at + 8 + 2] &= !1;
    assert!(Dictionary::parse(&corrupt).is_err());

    // Every single-bit mutation of the whole table parses-or-rejects without
    // panicking, and a parsed survivor still answers boundary queries.
    let table_len = 8 + 4;
    for index in table_at..table_at + table_len {
        for bit in 0..8 {
            let mut corrupt = bytes.clone();
            corrupt[index] ^= 1 << bit;
            let result = std::panic::catch_unwind(|| {
                if let Ok(dictionary) = Dictionary::parse(&corrupt) {
                    for right in 0..5u16 {
                        for left in 0..5u16 {
                            let _ = dictionary.bunsetsu_boundary(right, left);
                        }
                    }
                }
            });
            assert!(result.is_ok(), "bit {bit} of byte {index} panicked");
        }
    }
}
