use dictc::inflection::{expand_inflections, parse_inflection_pos_catalog};
use dictc::{compile, merge_entries, parse_connection, parse_entries, SourceEntry};
use sakura_core::conversion::{ConversionOptions, Converter};
use sakura_core::dictionary::Dictionary;
use sakura_core::ConversionMethod;

const POS: &str = "\
610 動詞,自立,*,*,カ変・来ル,基本形,来る
612 動詞,自立,*,*,カ変・来ル,未然ウ接続,来る
614 動詞,自立,*,*,カ変・来ル,未然形,来る
616 動詞,自立,*,*,カ変・来ル,連用形,来る
599 動詞,自立,*,*,カ変・来ル,仮定形,来る
606 動詞,自立,*,*,カ変・来ル,命令ｉ,来る
680 動詞,自立,*,*,一段,基本形,*
687 動詞,自立,*,*,一段,未然ウ接続,*
694 動詞,自立,*,*,一段,未然形,*
701 動詞,自立,*,*,一段,連用形,*
645 動詞,自立,*,*,一段,仮定形,*
666 動詞,自立,*,*,一段,命令ｒｏ,*
713 動詞,自立,*,*,一段・クレル,基本形,*
715 動詞,自立,*,*,一段・クレル,未然形,*
714 動詞,自立,*,*,一段・クレル,未然ウ接続,*
717 動詞,自立,*,*,一段・クレル,連用形,*
708 動詞,自立,*,*,一段・クレル,仮定形,*
711 動詞,自立,*,*,一段・クレル,命令ｒｏ,*
723 動詞,自立,*,*,五段・カ行イ音便,基本形,*
725 動詞,自立,*,*,五段・カ行イ音便,未然形,*
724 動詞,自立,*,*,五段・カ行イ音便,未然ウ接続,*
726 動詞,自立,*,*,五段・カ行イ音便,連用タ接続,*
727 動詞,自立,*,*,五段・カ行イ音便,連用形,*
720 動詞,自立,*,*,五段・カ行イ音便,仮定形,*
722 動詞,自立,*,*,五段・カ行イ音便,命令ｅ,*
731 動詞,自立,*,*,五段・カ行促音便,基本形,行く
733 動詞,自立,*,*,五段・カ行促音便,未然形,行く
732 動詞,自立,*,*,五段・カ行促音便,未然ウ接続,行く
734 動詞,自立,*,*,五段・カ行促音便,連用タ接続,行く
735 動詞,自立,*,*,五段・カ行促音便,連用形,行く
728 動詞,自立,*,*,五段・カ行促音便,仮定形,行く
730 動詞,自立,*,*,五段・カ行促音便,命令ｅ,行く
633 動詞,自立,*,*,サ変・スル,基本形,*
637 動詞,自立,*,*,サ変・スル,未然形,*
634 動詞,自立,*,*,サ変・スル,未然ウ接続,*
638 動詞,自立,*,*,サ変・スル,連用形,*
627 動詞,自立,*,*,サ変・スル,仮定形,*
631 動詞,自立,*,*,サ変・スル,命令ｒｏ,*
2425 形容詞,自立,*,*,形容詞・アウオ段,基本形,*
2398 形容詞,自立,*,*,形容詞・アウオ段,仮定形,*
2449 形容詞,自立,*,*,形容詞・アウオ段,連用タ接続,*
2454 形容詞,自立,*,*,形容詞・アウオ段,連用テ接続,*
";

fn lemma(reading: &str, surface: &str, class_id: u16, cost: i32) -> SourceEntry {
    let tsv = format!(
        "# license: LicenseRef-Mozc-Dictionary\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n{reading}\t{surface}\t{class_id}\t{class_id}\t{cost}\t{}\tpredict\t\n",
        cost + 1_200
    );
    parse_entries("lemma.tsv", &tsv)
        .expect("lemma parses")
        .remove(0)
}

fn catalog() -> dictc::inflection::InflectionPosCatalog {
    parse_inflection_pos_catalog("id.def", POS).expect("fixture POS catalog")
}

fn pair<'a>(entries: &'a [SourceEntry], reading: &str, surface: &str) -> &'a SourceEntry {
    entries
        .iter()
        .find(|entry| entry.reading == reading && entry.surface == surface)
        .unwrap_or_else(|| panic!("missing {reading} -> {surface}"))
}

#[test]
fn kuru_gains_the_everyday_te_ta_and_nai_forms() {
    let lemmas = vec![lemma("くる", "来る", 610, 7)];
    let (entries, report) = expand_inflections(&lemmas, &catalog()).expect("expand");
    assert!(report.emitted_entries > 0);
    let kite = pair(&entries, "きて", "来て");
    assert_eq!(kite.left_id, 616);
    assert_eq!(kite.right_id, 616);
    assert!(kite.word_cost < 100, "来て must outrank キテ-class noise");
    pair(&entries, "きた", "来た");
    pair(&entries, "こない", "来ない");
    pair(&entries, "きます", "来ます");
    pair(&entries, "こい", "来い");
    pair(&entries, "こよう", "来よう");
    pair(&entries, "きてない", "来てない");
    pair(&entries, "きている", "来ている");
}

#[test]
fn ichidan_and_godan_and_sa_and_adjective_forms_are_emitted() {
    let lemmas = vec![
        lemma("みる", "見る", 680, 100),
        lemma("きる", "着る", 680, 80),
        lemma("くれる", "呉れる", 713, 90),
        lemma("かく", "書く", 723, 50),
        lemma("いく", "行く", 731, 10),
        lemma("する", "する", 633, 20),
        lemma("たかい", "高い", 2425, 30),
    ];
    let (entries, _) = expand_inflections(&lemmas, &catalog()).expect("expand");
    pair(&entries, "みて", "見て");
    pair(&entries, "きて", "着て");
    pair(&entries, "くれて", "呉れて");
    pair(&entries, "かいて", "書いて");
    pair(&entries, "いって", "行って");
    pair(&entries, "して", "して");
    pair(&entries, "たかくて", "高くて");
    pair(&entries, "たかかった", "高かった");
    assert_eq!(pair(&entries, "みて", "見て").left_id, 701);
    assert_eq!(pair(&entries, "かいて", "書いて").left_id, 726);
    assert_eq!(pair(&entries, "いって", "行って").left_id, 734);
}

#[test]
fn existing_surfaces_are_not_duplicated() {
    let lemmas = vec![
        lemma("くる", "来る", 610, 7),
        lemma("きて", "来て", 616, 40),
    ];
    let (entries, report) = expand_inflections(&lemmas, &catalog()).expect("expand");
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.reading == "きて" && entry.surface == "来て")
            .count(),
        0
    );
    assert!(report.skipped_existing > 0);
    pair(&entries, "きた", "来た");
}

#[test]
fn compound_kuru_keeps_the_prefix() {
    let lemmas = vec![lemma("やってくる", "やって来る", 610, 200)];
    let (entries, _) = expand_inflections(&lemmas, &catalog()).expect("expand");
    pair(&entries, "やってきて", "やって来て");
    pair(&entries, "やってこない", "やって来ない");
}

#[test]
fn kite_converts_to_kite_kanji_after_expansion() {
    let lemmas = vec![lemma("くる", "来る", 610, 7)];
    let (inflected, _) = expand_inflections(&lemmas, &catalog()).expect("expand");
    let merged = merge_entries(lemmas, inflected).expect("merge");
    let matrix = parse_connection(
        "matrix.tsv",
        "# license: BSD-3-Clause\nclasses\t800\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    let bytes = compile(&merged, &matrix).expect("image");
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(
            &dictionary,
            "きて",
            ConversionOptions {
                max_candidates: 5,
                method: ConversionMethod::MultiSegment,
                it_bias_per_mille: 0,
                max_it_boost: 0,
                initial_right_id: 0,
                ..ConversionOptions::default()
            },
        )
        .expect("conversion");
    assert_eq!(candidates[0].text(), "来て");
}
