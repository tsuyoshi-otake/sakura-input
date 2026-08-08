use sakura_settings::formats::{
    decode_file_text, detect_format, encode_file_text, parse_dictionary, serialize_dictionary,
    DictionaryFormat,
};

struct Fixture {
    name: &'static str,
    source: &'static str,
    format: DictionaryFormat,
    windows_encoding: bool,
}

const FIXTURES: [Fixture; 3] = [
    Fixture {
        name: "Microsoft IME",
        source: include_str!("fixtures/ms-ime.txt"),
        format: DictionaryFormat::MicrosoftIme,
        windows_encoding: true,
    },
    Fixture {
        name: "ATOK",
        source: include_str!("fixtures/atok.txt"),
        format: DictionaryFormat::Atok,
        windows_encoding: true,
    },
    Fixture {
        name: "Mozc",
        source: include_str!("fixtures/mozc.txt"),
        format: DictionaryFormat::Mozc,
        windows_encoding: false,
    },
];

#[test]
fn external_fixture_import_and_export_roundtrip_without_field_loss() {
    for fixture in FIXTURES {
        assert_eq!(
            detect_format(fixture.source),
            Ok(fixture.format),
            "{}",
            fixture.name
        );
        let imported = parse_dictionary(fixture.source, fixture.format).expect(fixture.name);
        assert_eq!(imported.len(), 3, "{}", fixture.name);
        let sakura = imported
            .entries()
            .iter()
            .find(|entry| entry.reading == "さくら")
            .expect("sakura fixture row");
        assert_eq!(sakura.comment, "花の名前", "{}", fixture.name);

        let exported = serialize_dictionary(&imported, fixture.format);
        let bytes = encode_file_text(&exported, fixture.format);
        assert_eq!(
            bytes.starts_with(&[0xff, 0xfe]),
            fixture.windows_encoding,
            "{} encoding",
            fixture.name
        );
        let decoded = decode_file_text(&bytes).expect(fixture.name);
        let reparsed = parse_dictionary(&decoded, fixture.format).expect(fixture.name);
        assert_eq!(reparsed.entries(), imported.entries(), "{}", fixture.name);
    }
}

#[test]
fn each_fixture_can_cross_export_to_every_external_format() {
    for fixture in FIXTURES {
        let imported = parse_dictionary(fixture.source, fixture.format).expect(fixture.name);
        for destination in [
            DictionaryFormat::MicrosoftIme,
            DictionaryFormat::Atok,
            DictionaryFormat::Mozc,
        ] {
            let exported = serialize_dictionary(&imported, destination);
            let reparsed = parse_dictionary(&exported, destination)
                .unwrap_or_else(|error| panic!("{}: {error}", destination.name()));
            assert_eq!(
                reparsed.entries(),
                imported.entries(),
                "{} -> {}",
                fixture.name,
                destination.name()
            );
        }
    }
}
