use dictc_core::{
    compile, compile_with_details, parse_connection, parse_entries, SourceDetail,
    SourceDetailRelation,
};
use dictc_core::{glossary, wordnet};
use sakura_core::dictionary::{image_format, DetailRelationKind, Dictionary, Error};
use sakura_proto::FixedStr;
use std::ops::Range;

const ENTRIES: &str = "# license: MIT\n\
reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
よみ\t同形\t1\t1\t100\t-\t\t\n\
べつ\t同形\t1\t2\t200\t-\t\t\n";
const CONNECTION: &str = "# license: MIT\nclasses\t3\ndefault\t7\n";

fn image(with_details: bool) -> Vec<u8> {
    let entries = parse_entries("details.tsv", ENTRIES).expect("entries");
    let matrix = parse_connection("details-matrix.tsv", CONNECTION, false).expect("matrix");
    if !with_details {
        return compile(&entries, &matrix).expect("old compatible image");
    }
    compile_with_details(
        &entries,
        &matrix,
        &[SourceDetail {
            reading: "よみ".into(),
            surface: "同形".into(),
            left_id: 1,
            right_id: 1,
            description: format!("最初の文です。{}", "長文".repeat(12_000)),
            relations: vec![
                SourceDetailRelation {
                    kind: DetailRelationKind::Alias,
                    target: "別名".into(),
                },
                SourceDetailRelation {
                    kind: DetailRelationKind::Related,
                    target: "関連語".into(),
                },
            ],
        }],
    )
    .expect("detail image")
}

#[test]
fn detail_is_exact_entry_keyed_and_old_images_remain_compatible() {
    let bytes = image(true);
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut first = None;
    dictionary
        .common_prefix_search("よみ", |matched| {
            first = Some(matched.entry_index);
            false
        })
        .expect("first lookup");
    let detail = dictionary
        .detail_at(first.expect("entry ordinal"))
        .expect("detail lookup")
        .expect("exact detail");
    let mut full = String::new();
    detail
        .write_description(&mut full)
        .expect("full description");
    assert_eq!(
        full.encode_utf16().count(),
        "最初の文です。".encode_utf16().count() + 24_000
    );
    let mut display = String::new();
    detail
        .write_display_description(&mut display)
        .expect("display description");
    assert_eq!(
        display, full,
        "dictionary does not shorten source definitions"
    );
    let mut preview = FixedStr::<31>::new();
    assert!(detail
        .write_description_preview(&mut preview, 30)
        .expect("bounded preview"));
    assert!(preview.as_str().is_char_boundary(preview.len()));
    assert!(preview.len() <= 30);
    let mut relations = Vec::new();
    detail
        .visit_relations(|kind, text| {
            relations.push((kind, text.to_owned()));
            true
        })
        .expect("relations");
    assert_eq!(
        relations,
        [
            (DetailRelationKind::Alias, "別名".to_owned()),
            (DetailRelationKind::Related, "関連語".to_owned())
        ]
    );

    let mut collision = None;
    dictionary
        .common_prefix_search("べつ", |matched| {
            collision = Some(matched.entry_index);
            false
        })
        .expect("collision lookup");
    assert!(
        dictionary
            .detail_at(collision.expect("collision ordinal"))
            .expect("collision detail lookup")
            .is_none(),
        "a same-surface entry with a different exact identity must not inherit details"
    );

    let old_bytes = image(false);
    let old = Dictionary::parse(&old_bytes).expect("old dictionary");
    assert!(old.detail_at(0).expect("old image lookup").is_none());
}

#[test]
fn merged_glossary_and_wordnet_details_reach_the_runtime() {
    let entries = parse_entries(
        "merged.tsv",
        "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
どっかー\tDocker\t1\t1\t100\t-\t\t\n\
ねこ\t猫\t2\t2\t100\t-\t\t\n",
    )
    .expect("entries");
    let glossary_terms = glossary::parse_part(
        "part.json",
        r#"[{"term":"Docker","aliases":["docker"],"senses":[{"definition":"container platform","keywords":["Container"]}]},{"term":"Container","senses":[{"definition":"target"}]}]"#,
    )
    .expect("glossary terms");
    let mut details = glossary::detail_sources(&glossary_terms, &entries);
    let wordnet_xml = r#"<LexicalResource><Lexicon>
<LexicalEntry><Lemma writtenForm="猫"/><Sense id="cat" synset="s1"/></LexicalEntry>
<LexicalEntry><Lemma writtenForm="ねこ"/><Sense id="cat-kana" synset="s1"/></LexicalEntry>
<Synset id="s1"><Definition gloss="cat definition"/></Synset>
</Lexicon></LexicalResource>"#;
    details.extend(
        wordnet::import_lmf(std::io::BufReader::new(wordnet_xml.as_bytes()), &entries)
            .expect("WordNet import")
            .details,
    );
    let matrix = parse_connection("matrix", CONNECTION, false).expect("matrix");
    let image = compile_with_details(&entries, &matrix, &details).expect("dictionary image");
    let dictionary = Dictionary::parse(&image).expect("dictionary");

    let mut docker = None;
    dictionary
        .common_prefix_search("どっかー", |matched| {
            docker = Some(matched.entry_index);
            false
        })
        .expect("Docker lookup");
    let docker = dictionary
        .detail_at(docker.expect("Docker ordinal"))
        .expect("Docker detail lookup")
        .expect("glossary detail");
    let mut docker_relations = Vec::new();
    docker
        .visit_relations(|kind, target| {
            docker_relations.push((kind, target.to_owned()));
            true
        })
        .expect("glossary relations");
    assert!(docker_relations.contains(&(DetailRelationKind::Alias, "docker".into())));
    assert!(docker_relations.contains(&(DetailRelationKind::Related, "Container".into())));

    let mut cat = None;
    dictionary
        .common_prefix_search("ねこ", |matched| {
            cat = Some(matched.entry_index);
            false
        })
        .expect("cat lookup");
    let cat = dictionary
        .detail_at(cat.expect("cat ordinal"))
        .expect("cat detail lookup")
        .expect("WordNet detail");
    let mut cat_relations = Vec::new();
    cat.visit_relations(|kind, target| {
        cat_relations.push((kind, target.to_owned()));
        true
    })
    .expect("WordNet relations");
    assert!(cat_relations.contains(&(DetailRelationKind::Synonym, "ねこ".into())));
}

#[test]
fn malformed_optional_detail_tables_are_rejected_without_panics() {
    let bytes = image(true);
    let table_count = u16::from_le_bytes([bytes[12], bytes[13]]) as usize;
    let directory_at = 32usize;
    let detail_tag = *b"DREL";
    let relation_directory = (0..table_count)
        .map(|index| directory_at + index * 16)
        .find(|at| bytes[*at..*at + 4] == detail_tag)
        .expect("detail relation directory");

    let mut partial = bytes.clone();
    partial[relation_directory..relation_directory + 4].copy_from_slice(b"XREL");
    assert!(
        Dictionary::parse(&partial).is_err(),
        "partial detail family"
    );

    for index in 0..bytes.len() {
        let mut corrupt = bytes.clone();
        corrupt[index] ^= 0xff;
        let outcome = std::panic::catch_unwind(|| {
            if let Ok(dictionary) = Dictionary::parse(&corrupt) {
                for ordinal in 0..dictionary.entry_count() {
                    if let Ok(Some(detail)) = dictionary.detail_at(ordinal) {
                        let mut text = String::new();
                        let _ = detail.write_description(&mut text);
                        let _ = detail.visit_relations(|_, _| true);
                    }
                }
            }
        });
        assert!(outcome.is_ok(), "mutation at byte {index} panicked");
    }
}

/// A detail image whose description and relation targets are short, so a test
/// can corrupt every byte of its text tables.
fn short_text_image() -> Vec<u8> {
    let entries = parse_entries("details.tsv", ENTRIES).expect("entries");
    let matrix = parse_connection("details-matrix.tsv", CONNECTION, false).expect("matrix");
    compile_with_details(
        &entries,
        &matrix,
        &[SourceDetail {
            reading: "よみ".into(),
            surface: "同形".into(),
            left_id: 1,
            right_id: 1,
            description: "説明 A 😀 文".into(),
            relations: vec![
                SourceDetailRelation {
                    kind: DetailRelationKind::Alias,
                    target: "別名".into(),
                },
                SourceDetailRelation {
                    kind: DetailRelationKind::Related,
                    target: "関連語".into(),
                },
            ],
        }],
    )
    .expect("short detail image")
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
}

fn table_range(image: &[u8], tag: [u8; 4]) -> Range<usize> {
    let table_count = usize::from(u16::from_le_bytes([image[12], image[13]]));
    let at = (0..table_count)
        .map(|index| image_format::HEADER_LEN + index * image_format::DIRECTORY_ENTRY_LEN)
        .find(|at| image[*at..*at + 4] == tag)
        .expect("table in the directory");
    let start = u32_at(image, at + 4) as usize;
    start..start + u32_at(image, at + 8) as usize
}

fn parse_outcome(image: &[u8]) -> Result<(), Error> {
    Dictionary::parse(image).map(|_| ())
}

#[test]
fn an_invalid_detail_text_is_rejected_when_the_image_is_opened() {
    let mut bytes = short_text_image();
    let texts = table_range(&bytes, image_format::TAG_DETAIL_TEXT);
    bytes[texts.start] = 0xff;
    assert_eq!(parse_outcome(&bytes), Err(Error::BadUtf8));
}

#[test]
fn a_detail_text_that_starts_inside_a_character_is_rejected_when_the_image_is_opened() {
    let mut bytes = short_text_image();
    let offsets = table_range(&bytes, image_format::TAG_DETAIL_TEXT_OFFSETS);
    let second = offsets.start + 4;
    let start = u32_at(&bytes, second);
    bytes[second..second + 4].copy_from_slice(&(start + 1).to_le_bytes());
    let texts = table_range(&bytes, image_format::TAG_DETAIL_TEXT);
    assert!(
        std::str::from_utf8(&bytes[texts]).is_ok(),
        "only the split moved; the table is still valid UTF-8 as a whole"
    );
    assert_eq!(parse_outcome(&bytes), Err(Error::BadUtf8));
}

#[test]
fn a_detail_text_nothing_refers_to_is_not_examined() {
    // Opening follows the references, as it did before the table was checked
    // in one pass, so an image that only damages an unused text keeps opening.
    let mut bytes = short_text_image();
    let relations = table_range(&bytes, image_format::TAG_DETAIL_RELATIONS);
    let first_target = relations.start + 4;
    let second_target = relations.start + image_format::DETAIL_RELATION_LEN + 4;
    let orphan = u32_at(&bytes, second_target) as usize;
    bytes.copy_within(first_target..first_target + 4, second_target);
    let offsets = table_range(&bytes, image_format::TAG_DETAIL_TEXT_OFFSETS);
    let texts = table_range(&bytes, image_format::TAG_DETAIL_TEXT);
    let orphan_start = u32_at(&bytes, offsets.start + 4 * orphan) as usize;
    bytes[texts.start + orphan_start] = 0xff;
    assert_eq!(parse_outcome(&bytes), Ok(()));
}

/// The judgement opening must reach, written per text. Every text of
/// `short_text_image` is referenced, so each one has to be valid on its own.
fn per_text_outcome(image: &[u8]) -> Result<(), Error> {
    let offsets = table_range(image, image_format::TAG_DETAIL_TEXT_OFFSETS);
    let data = &image[table_range(image, image_format::TAG_DETAIL_TEXT)];
    let starts: Vec<usize> = image[offsets]
        .chunks_exact(4)
        .map(|chunk| u32_at(chunk, 0) as usize)
        .collect();
    if starts.iter().any(|start| *start >= data.len()) || starts.windows(2).any(|w| w[0] >= w[1]) {
        return Err(Error::BadTable(image_format::TAG_DETAIL_TEXT));
    }
    let ends = starts.iter().skip(1).copied().chain([data.len()]);
    if starts
        .iter()
        .copied()
        .zip(ends)
        .all(|(start, end)| std::str::from_utf8(&data[start..end]).is_ok())
    {
        Ok(())
    } else {
        Err(Error::BadUtf8)
    }
}

#[test]
fn every_corruption_of_the_detail_text_tables_is_judged_per_text() {
    let image = short_text_image();
    assert_eq!(per_text_outcome(&image), Ok(()));
    let mut seen = std::collections::BTreeSet::new();
    for table in [
        image_format::TAG_DETAIL_TEXT_OFFSETS,
        image_format::TAG_DETAIL_TEXT,
    ] {
        for at in table_range(&image, table) {
            for mask in 1..=u8::MAX {
                let mut corrupt = image.clone();
                corrupt[at] ^= mask;
                let expected = per_text_outcome(&corrupt);
                assert_eq!(
                    parse_outcome(&corrupt),
                    expected,
                    "byte {at} xor {mask:#04x}"
                );
                seen.insert(format!("{expected:?}"));
            }
        }
    }
    assert_eq!(
        seen.len(),
        3,
        "the sweep must reach accepted, bad UTF-8 and bad offsets: {seen:?}"
    );
}

#[test]
fn a_detail_text_id_past_the_text_table_is_rejected_even_when_every_text_is_valid() {
    // Every text is valid, so opening takes the one-pass check; the id of each
    // reference is still judged by itself.
    let image = short_text_image();
    let text_count = (table_range(&image, image_format::TAG_DETAIL_TEXT_OFFSETS).len() / 4) as u32;
    let records = table_range(&image, image_format::TAG_DETAIL_RECORDS);
    let relations = table_range(&image, image_format::TAG_DETAIL_RELATIONS);
    for (at, tag) in [
        (records.start, image_format::TAG_DETAIL_RECORDS),
        (records.start + 4, image_format::TAG_DETAIL_RECORDS),
        (relations.start + 4, image_format::TAG_DETAIL_RELATIONS),
    ] {
        let mut corrupt = image.clone();
        corrupt[at..at + 4].copy_from_slice(&text_count.to_le_bytes());
        assert_eq!(
            parse_outcome(&corrupt),
            Err(Error::BadTable(tag)),
            "text id at byte {at}"
        );
    }
}
