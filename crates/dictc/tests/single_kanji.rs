//! The optional single-kanji tables: compile-side encoding, reader validation,
//! and the lookups conversion appends candidates from.
//!
//! The fixture mirrors the real shape of the pinned source: a one-mora reading
//! naming many characters, a longer reading naming few, and a variant rule that
//! annotates one of them. It also covers what the pinned source actually
//! contains that a naive encoder would corrupt — characters outside the BMP,
//! and readings whose byte order differs from their scalar order.

use dictc::single_kanji::SingleKanjiTable;
use dictc::{compile, compile_with_tables, parse_connection, parse_entries, OptionalTables};
use sakura_core::dictionary::{Dictionary, SingleKanjiVariantKind};

const ENTRIES: &str = "# license: BSD-3-Clause\n\
reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
ひ\t日\t1\t1\t100\t-\t\t\n\
こう\t高\t1\t1\t100\t-\t\t\n";

const CONNECTION: &str = "# license: BSD-3-Clause\n\
classes\t2\n\
default\t0\n";

// Byte order over UTF-8 hiragana is not the same as the order these readings
// are written in here, so an encoder that trusted input order would produce an
// index the reader's binary search silently misses.
const SINGLE_KANJI: &str = "ひ\t日火比\n\
こう\t口工高\n\
あ\t亜\n\
しか\t𠮟叱\n\
ん\t\u{3093}\n";

const VARIANTS: &str = "# SingleKanji variant rule\n\
異体字\n\
髙\t高\n\
\n\
旧字体\n\
緣\t縁\n\
𠮟\t叱\n";

fn table() -> SingleKanjiTable {
    SingleKanjiTable::build(
        "single_kanji.tsv",
        SINGLE_KANJI,
        "variant_rule.txt",
        VARIANTS,
    )
    .expect("single-kanji table")
}

fn image() -> Vec<u8> {
    let entries = parse_entries("fixture.tsv", ENTRIES).expect("entries");
    let connection = parse_connection("connection.tsv", CONNECTION, false).expect("matrix");
    compile_with_tables(
        &entries,
        &connection,
        OptionalTables {
            single_kanji: Some(&table()),
            ..OptionalTables::default()
        },
    )
    .expect("image with single kanji")
}

#[test]
fn every_reading_round_trips_in_source_preference_order() {
    let bytes = image();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    assert!(dictionary.has_single_kanji());
    let listed = |reading: &str| dictionary.single_kanji(reading).collect::<Vec<_>>();
    assert_eq!(listed("ひ"), ['日', '火', '比']);
    assert_eq!(listed("こう"), ['口', '工', '高']);
    assert_eq!(listed("あ"), ['亜']);
    assert_eq!(listed("ん"), ['\u{3093}']);
}

#[test]
fn a_reading_the_table_does_not_list_yields_nothing() {
    let bytes = image();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    for reading in ["", "き", "ひひ", "こ", "こうこう", "日", "hi"] {
        assert_eq!(
            dictionary.single_kanji(reading).count(),
            0,
            "reading {reading:?} must not resolve"
        );
        assert_eq!(dictionary.single_kanji_count(reading), 0);
    }
}

#[test]
fn a_reading_that_is_a_prefix_of_another_does_not_borrow_its_characters() {
    // Binary search over byte-ordered readings puts こう immediately after こ
    // would sort; a comparison that stopped at the shorter length would return
    // こう's characters for こ.
    let bytes = image();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    assert_eq!(dictionary.single_kanji_count("こ"), 0);
    assert_eq!(dictionary.single_kanji_count("こう"), 3);
}

#[test]
fn characters_outside_the_basic_plane_survive_the_round_trip() {
    let bytes = image();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let listed = dictionary.single_kanji("しか").collect::<Vec<_>>();
    assert_eq!(listed, ['𠮟', '叱']);
    assert!(
        listed[0] as u32 > 0xFFFF,
        "fixture must exercise a surrogate pair"
    );
}

#[test]
fn variant_notes_carry_their_original_and_kind() {
    let bytes = image();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let itaiji = dictionary
        .single_kanji_variant('髙')
        .expect("髙 has a rule");
    assert_eq!(itaiji.original, '高');
    assert_eq!(itaiji.kind, SingleKanjiVariantKind::Itaiji);
    assert_eq!(itaiji.kind.label(), "異体字");

    let old = dictionary
        .single_kanji_variant('緣')
        .expect("緣 has a rule");
    assert_eq!(old.original, '縁');
    assert_eq!(old.kind, SingleKanjiVariantKind::OldForm);

    // The rule list is searched by scalar value, so a non-BMP variant must be
    // found at its real code point rather than at its first surrogate.
    let non_bmp = dictionary
        .single_kanji_variant('𠮟')
        .expect("𠮟 has a rule");
    assert_eq!(non_bmp.original, '叱');
}

#[test]
fn a_character_without_a_rule_has_no_note() {
    let bytes = image();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    for character in ['日', '火', '高', '縁', '叱', 'あ', 'A'] {
        assert!(
            dictionary.single_kanji_variant(character).is_none(),
            "{character} must not carry a note"
        );
    }
}

#[test]
fn an_image_without_the_table_reports_no_single_kanji() {
    let entries = parse_entries("fixture.tsv", ENTRIES).expect("entries");
    let connection = parse_connection("connection.tsv", CONNECTION, false).expect("matrix");
    let bytes = compile(&entries, &connection).expect("image");
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    assert!(!dictionary.has_single_kanji());
    assert_eq!(dictionary.single_kanji_count("ひ"), 0);
    assert!(dictionary.single_kanji_variant('髙').is_none());
}

#[test]
fn the_encoded_tables_are_byte_deterministic() {
    assert_eq!(image(), image());
}

#[test]
fn every_variant_kind_round_trips_through_its_stored_code() {
    use SingleKanjiVariantKind as Kind;

    for kind in [
        Kind::Itaiji,
        Kind::PrintStandard,
        Kind::SimplifiedConventional,
        Kind::OldForm,
        Kind::Abbreviated,
        Kind::OrthodoxForm,
        Kind::PopularForm,
        Kind::DistinctCharacter,
        Kind::OriginalForm,
    ] {
        assert_eq!(Kind::from_code(kind.code()), Some(kind));
        assert!(!kind.label().is_empty());
    }
    // Zero is reserved so a zeroed record cannot decode as a real relation.
    assert_eq!(Kind::from_code(0), None);
    assert_eq!(Kind::from_code(10), None);
    assert_eq!(Kind::from_code(u8::MAX), None);
}

#[test]
fn a_corrupt_table_is_rejected_rather_than_read() {
    let original = image();
    let index_at = find_table(&original, *b"SKIX");
    let variants_at = find_table(&original, *b"SKVR");
    let chars_at = find_table(&original, *b"SKCH");

    // Every mutation below breaks exactly one invariant the lookups rely on.
    let corruptions: [(usize, [u8; 4], &str); 4] = [
        (
            index_at + 4,
            [0xFF, 0xFF, 0xFF, 0x7F],
            "character span out of range",
        ),
        (
            index_at,
            [0xFF, 0xFF, 0xFF, 0x7F],
            "reading span out of range",
        ),
        (
            variants_at,
            [0xFF, 0xFF, 0xFF, 0xFF],
            "variant is not a scalar value",
        ),
        (
            chars_at,
            [0xFF, 0xFF, 0xFF, 0xFF],
            "character is not a scalar value",
        ),
    ];
    for (offset, bytes, what) in corruptions {
        let mut corrupt = original.clone();
        corrupt[offset..offset + 4].copy_from_slice(&bytes);
        assert!(
            Dictionary::parse(&corrupt).is_err(),
            "parse must reject a {what}"
        );
    }

    // Readings must ascend strictly, so swapping two index records is rejected
    // even though every individual record stays well formed.
    let mut swapped = original.clone();
    let (first, second) = (index_at, index_at + 12);
    let mut record = [0u8; 12];
    record.copy_from_slice(&swapped[first..first + 12]);
    swapped.copy_within(second..second + 12, first);
    swapped[second..second + 12].copy_from_slice(&record);
    assert!(
        Dictionary::parse(&swapped).is_err(),
        "parse must reject an unsorted index"
    );
}

/// Returns the image offset of the named table's payload.
fn find_table(image: &[u8], tag: [u8; 4]) -> usize {
    let table_count = usize::from(u16::from_le_bytes([image[12], image[13]]));
    for index in 0..table_count {
        let at = 32 + index * 16;
        if image[at..at + 4] == tag {
            return u32::from_le_bytes(image[at + 4..at + 8].try_into().expect("offset")) as usize;
        }
    }
    panic!("image has no {} table", String::from_utf8_lossy(&tag));
}
