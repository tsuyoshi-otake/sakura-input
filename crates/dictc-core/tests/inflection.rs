use dictc_core::inflection::{expand_inflections, parse_inflection_pos_catalog};
use dictc_core::{compile, merge_entries, parse_connection, parse_entries, SourceEntry};
use sakura_core::conversion::{ConversionOptions, Converter};
use sakura_core::dictionary::{Dictionary, EntryFlags};
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
833 動詞,自立,*,*,五段動詞,仮定形,*
836 動詞,自立,*,*,五段動詞,命令ｅ,*
837 動詞,自立,*,*,五段動詞,基本形,*
838 動詞,自立,*,*,五段動詞,未然ウ接続,*
839 動詞,自立,*,*,五段動詞,未然形,*
841 動詞,自立,*,*,五段動詞,連用タ接続,*
842 動詞,自立,*,*,五段動詞,連用形,*
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

fn catalog() -> dictc_core::inflection::InflectionPosCatalog {
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

/// Auxiliary rows copied from Mozc id.def (ids 1097, 1099, 1084, 204).
const AUX_POS: &str = "\
1097 動詞,非自立,*,*,一段,基本形,てる
1099 動詞,非自立,*,*,一段,基本形,でる
1084 動詞,非自立,*,*,一段,基本形,いる
204 助動詞,*,*,*,特殊・ナイ,基本形,ない
";

fn catalog_with_auxiliaries() -> dictc_core::inflection::InflectionPosCatalog {
    parse_inflection_pos_catalog("id.def", &format!("{POS}{AUX_POS}"))
        .expect("POS with auxiliaries")
}

#[test]
fn contracted_te_forms_connect_as_their_auxiliary_and_cost_more_than_the_split() {
    // Issue #334: a fused form that is as cheap as its lemma and connects as the
    // stem absorbs the particle before it (これであってる -> これ出会ってる).
    let lemmas = vec![lemma("くる", "来る", 610, 7)];
    let (entries, _) = expand_inflections(&lemmas, &catalog_with_auxiliaries()).expect("expand");
    for (reading, surface, aux_id, cost_delta) in [
        ("きてる", "来てる", 1097_u16, 2_035),
        ("きている", "来ている", 1084, 2_040),
        ("きてない", "来てない", 204, 2_845),
    ] {
        let form = pair(&entries, reading, surface);
        assert_eq!(form.left_id, 616, "{reading}: left id stays the stem's");
        assert_eq!(
            form.right_id, aux_id,
            "{reading}: right id is the auxiliary's"
        );
        assert_eq!(
            form.word_cost,
            7 + cost_delta,
            "{reading}: conversion delta is 2,035 / 2,040 / 2,845"
        );
    }
}

#[test]
fn contracted_te_forms_keep_the_stem_id_when_the_catalog_has_no_auxiliary() {
    // The fixture catalog in this file has no 非自立 / ない rows: fall back to the
    // stem id instead of dropping the form.
    let lemmas = vec![lemma("くる", "来る", 610, 7)];
    let (entries, _) = expand_inflections(&lemmas, &catalog()).expect("expand");
    for (reading, surface) in [
        ("きてる", "来てる"),
        ("きている", "来ている"),
        ("きてない", "来てない"),
    ] {
        let form = pair(&entries, reading, surface);
        assert_eq!(form.left_id, 616);
        assert_eq!(form.right_id, 616, "{reading}: fallback keeps the stem id");
    }
}

#[test]
fn raising_the_conversion_cost_leaves_prediction_of_te_forms_unchanged() {
    // When the prediction flag follows the raised word cost, generated rows whose
    // cost passes 6,000 lose `predict` and prefix prediction of forms such as
    // しってる -> 知ってる disappears. Prediction cost and flag keep following
    // the lemma cost plus the former delta (35 / 40 / 45).
    let lemmas = vec![lemma("くる", "来る", 610, 5_000)];
    let (entries, _) = expand_inflections(&lemmas, &catalog_with_auxiliaries()).expect("expand");
    for (reading, surface, former_delta) in [
        ("きてる", "来てる", 35),
        ("きている", "来ている", 40),
        ("きてない", "来てない", 45),
    ] {
        let form = pair(&entries, reading, surface);
        assert!(
            form.flags.contains(EntryFlags::PREDICTION),
            "{reading}: still a prediction entry although the word cost is above 6,000"
        );
        assert_eq!(
            form.prediction_cost,
            5_000 + former_delta + 1_200,
            "{reading}"
        );
    }
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
fn adjective_phrases_whose_spelling_does_not_end_in_i_do_not_conjugate() {
    // Issue #354: Mozc files phrases such as 難しい場合 under the adjective
    // class. Their reading ends in い (ばあい) but the spelling in the kanji 合,
    // so the ending went onto the phrase: むずかしいばあくて -> 難しい場合て.
    let lemmas = vec![
        lemma("むずかしいばあい", "難しい場合", 2425, 3_000),
        lemma("ないせかい", "ない世界", 2425, 3_000),
        lemma("たかい", "高い", 2425, 30),
    ];
    let (entries, _) = expand_inflections(&lemmas, &catalog()).expect("expand");
    let phrase_forms = entries
        .iter()
        .filter(|entry| {
            entry.surface.starts_with("難しい場合") || entry.surface.starts_with("ない世界")
        })
        .map(|entry| (entry.reading.as_str(), entry.surface.as_str()))
        .collect::<Vec<_>>();
    assert!(
        phrase_forms.is_empty(),
        "phrase lemmas conjugated: {phrase_forms:?}"
    );
    pair(&entries, "たかくて", "高くて");
    pair(&entries, "たかければ", "高ければ");
}

#[test]
fn generic_godan_lemmas_conjugate_by_the_kana_that_ends_the_reading() {
    // Issue #348: Mozc OSS files every godan verb without a class of its own
    // under the generic 五段動詞 class (ids 833-842), so す/む/つ/ぐ/ぶ/ぬ verbs sit
    // next to る verbs on id 837. Only ラ行 endings were generated: 待って, 読んで,
    // 遊んで and 話して were missing and まってる converted to 舞ってる.
    let lemmas = vec![
        lemma("まつ", "待つ", 837, 100),
        lemma("よむ", "読む", 837, 100),
        lemma("よぶ", "呼ぶ", 837, 100),
        lemma("およぐ", "泳ぐ", 837, 100),
        lemma("しぬ", "死ぬ", 837, 100),
        lemma("はなす", "話す", 837, 100),
        lemma("とる", "取る", 837, 100),
    ];
    let (entries, report) = expand_inflections(&lemmas, &catalog()).expect("expand");
    for (reading, surface, left_id) in [
        ("まって", "待って", 841_u16),
        ("まった", "待った", 841),
        ("まったら", "待ったら", 841),
        ("またない", "待たない", 839),
        ("まちます", "待ちます", 842),
        ("まちたい", "待ちたい", 842),
        ("まてば", "待てば", 833),
        ("まとう", "待とう", 838),
        ("まて", "待て", 836),
        ("まってる", "待ってる", 841),
        ("まっている", "待っている", 841),
        ("まってない", "待ってない", 841),
        ("よんで", "読んで", 841),
        ("よんだ", "読んだ", 841),
        ("よまない", "読まない", 839),
        ("よみます", "読みます", 842),
        ("よんで", "呼んで", 841),
        ("よばない", "呼ばない", 839),
        ("およいで", "泳いで", 841),
        ("およいだ", "泳いだ", 841),
        ("しんで", "死んで", 841),
        ("はなして", "話して", 841),
        ("はなさない", "話さない", 839),
        ("はなします", "話します", 842),
        ("とって", "取って", 841),
    ] {
        let form = pair(&entries, reading, surface);
        assert_eq!(form.left_id, left_id, "{reading} -> {surface}");
        assert_eq!(
            form.word_cost,
            100 + form_delta(reading),
            "{reading} -> {surface}"
        );
    }
    assert_eq!(report.skipped_unsupported, 0);
}

/// Conversion cost delta of a generated form (inflection.rs FormSpec table).
fn form_delta(reading: &str) -> i32 {
    if reading.ends_with("てる") || reading.ends_with("でる") {
        2_035
    } else if reading.ends_with("ている") || reading.ends_with("でいる") {
        2_040
    } else if reading.ends_with("てない") || reading.ends_with("でない") {
        2_845
    } else if reading.ends_with("たら") || reading.ends_with("だら") {
        30
    } else if reading.ends_with("って")
        || reading.ends_with("んで")
        || reading.ends_with("いで")
        || reading.ends_with("して")
    {
        20
    } else if reading.ends_with("った") || reading.ends_with("んだ") || reading.ends_with("いだ")
    {
        25
    } else if reading.ends_with("ない") {
        30
    } else if reading.ends_with("ます") {
        40
    } else if reading.ends_with("たい") {
        45
    } else if reading.ends_with("ば") {
        35
    } else if reading.ends_with("う") {
        40
    } else {
        60
    }
}

#[test]
fn generic_godan_lemmas_without_a_godan_ending_stay_unsupported() {
    // Rows whose reading ends on a kana that no godan row owns (noise filed
    // under id 837) emit nothing and are counted, as before.
    let lemmas = vec![lemma("ぼたん", "ボタン", 837, 100)];
    let (entries, report) = expand_inflections(&lemmas, &catalog()).expect("expand");
    assert!(entries.is_empty());
    assert_eq!(report.skipped_unsupported, 1);
}

#[test]
fn spellings_without_okurigana_for_the_ending_stay_unsupported() {
    // A katakana or mixed spelling ends on no hiragana, so no okurigana
    // carries the dictionary ending. Nothing can be replaced, and the only
    // "forms" would glue the new ending onto the whole word (ムス + て).
    let lemmas = vec![
        lemma("むす", "ムス", 837, 100),
        lemma("あるある", "あるアル", 837, 100),
    ];
    let (entries, report) = expand_inflections(&lemmas, &catalog()).expect("expand");
    let emitted: Vec<(&str, &str)> = entries
        .iter()
        .map(|entry| (entry.reading.as_str(), entry.surface.as_str()))
        .collect();
    assert!(emitted.is_empty(), "{emitted:?}");
    assert_eq!(report.skipped_unsupported, 2);
}

#[test]
fn generated_forms_never_undercut_a_native_sibling_form_of_a_cheaper_lemma() {
    // Issue #390: Mozc prices its own fused rows far above the lemma (押す
    // 2,716 but 押したら 4,762) while a generated form costs the lemma plus a
    // small delta, so 推したら (推す 4,209 + 30) undercut 押したら. A generated
    // form stays behind every native sibling form of its reading whose lemma
    // is at least as common as its own; prediction climbs by the same amount
    // for a form that predicts (た; たら leaves prediction, Issue #400).
    let lemmas = vec![
        lemma("おす", "押す", 837, 2_716),
        lemma("おす", "推す", 837, 4_209),
        lemma("おしたら", "押したら", 842, 4_762),
        lemma("おした", "押した", 842, 4_500),
    ];
    let (entries, report) = expand_inflections(&lemmas, &catalog()).expect("expand");
    let raised = pair(&entries, "おしたら", "推したら");
    assert_eq!(raised.word_cost, 4_763);
    assert_eq!(raised.prediction_cost, i32::MAX);
    let raised = pair(&entries, "おした", "推した");
    assert_eq!(raised.word_cost, 4_501);
    assert_eq!(raised.prediction_cost, 4_501 + 1_200);
    assert_eq!(pair(&entries, "おして", "推して").word_cost, 4_209 + 20);
    assert_eq!(pair(&entries, "おして", "押して").word_cost, 2_716 + 20);
    assert!(!entries
        .iter()
        .any(|entry| entry.reading == "おしたら" && entry.surface == "押したら"));
    assert_eq!(report.raised_to_native, 2);
    assert_eq!(report.kept_under_rarer_native, 0);
}

#[test]
fn the_floor_is_the_dearest_qualifying_native_and_can_leave_prediction() {
    // Two native siblings of cheaper lemmas: the generated form lands above
    // both, and a floor past the prediction limit drops the predict flag.
    let lemmas = vec![
        lemma("おす", "押す", 837, 2_716),
        lemma("おす", "捺す", 837, 4_000),
        lemma("おす", "推す", 837, 4_209),
        lemma("おしたら", "押したら", 842, 4_762),
        lemma("おしたら", "捺したら", 842, 6_000),
    ];
    let (entries, report) = expand_inflections(&lemmas, &catalog()).expect("expand");
    let raised = pair(&entries, "おしたら", "推したら");
    assert_eq!(raised.word_cost, 6_001);
    assert_eq!(raised.prediction_cost, i32::MAX);
    assert_eq!(raised.flags, EntryFlags::NONE);
    assert_eq!(report.raised_to_native, 1);
}

#[test]
fn a_native_form_of_a_rarer_lemma_leaves_the_common_sibling_alone() {
    // 詠んで is a native row while 読んで is generated; 詠む (5,000) is the
    // rarer lemma, so 読んで keeps 読む's price instead of climbing past 詠んで.
    let lemmas = vec![
        lemma("よむ", "読む", 837, 2_121),
        lemma("よむ", "詠む", 837, 5_000),
        lemma("よんで", "詠んで", 841, 5_100),
    ];
    let (entries, report) = expand_inflections(&lemmas, &catalog()).expect("expand");
    assert_eq!(pair(&entries, "よんで", "読んで").word_cost, 2_121 + 20);
    assert_eq!(report.raised_to_native, 0);
    assert_eq!(report.kept_under_rarer_native, 1);
}

#[test]
fn a_native_row_the_generator_does_not_derive_sets_no_floor() {
    // 済まない is filed as an i-adjective lemma, not as a form of 住む, and
    // 不毛 is a noun: neither is a sibling form, so 住まない and 踏もう keep
    // their lemma's price.
    let lemmas = vec![
        lemma("すむ", "住む", 837, 2_785),
        lemma("すまない", "済まない", 2425, 3_987),
        lemma("ふむ", "踏む", 837, 4_063),
        lemma("ふもう", "不毛", 1931, 4_762),
    ];
    let (entries, report) = expand_inflections(&lemmas, &catalog()).expect("expand");
    assert_eq!(pair(&entries, "すまない", "住まない").word_cost, 2_785 + 30);
    assert_eq!(pair(&entries, "ふもう", "踏もう").word_cost, 4_063 + 40);
    assert_eq!(report.raised_to_native, 0);
    assert_eq!(report.kept_under_rarer_native, 0);
}

#[test]
fn a_native_row_of_another_class_with_the_same_spelling_sets_no_floor() {
    // The kana lemma みる (cost 0 here, like the auxiliary みる) has the
    // た-form みた, and Mozc has a row みた, but that row is the 五段 未然形 of
    // 満つ, not an 一段 form: matching on spelling alone priced 見た (1,521) at
    // 5,017. The same holds for a native row of the right family but another
    // 活用形 (the 一段 未然形 いて is no て-form).
    let lemmas = vec![
        lemma("みる", "見る", 680, 1_496),
        lemma("みる", "みる", 680, 0),
        lemma("みた", "みた", 839, 5_016),
        lemma("いる", "射る", 680, 4_800),
        lemma("いる", "いる", 680, 0),
        lemma("いて", "いて", 694, 5_013),
    ];
    let (entries, report) = expand_inflections(&lemmas, &catalog()).expect("expand");
    assert_eq!(pair(&entries, "みた", "見た").word_cost, 1_496 + 25);
    assert_eq!(pair(&entries, "いて", "射て").word_cost, 4_800 + 20);
    assert_eq!(report.raised_to_native, 0);
}

#[test]
fn kana_lemma_forms_leave_prediction_to_an_as_common_kanji_lemma() {
    // Issue #392: kana lemmas conjugate too. When a kanji lemma of the same
    // reading and POS id costs no more (出る 41 next to でる 1,269), the kana
    // forms stay convertible but leave prediction, so で… completes 出て and
    // not でて. Where Mozc prices the kana lemma lower (くださる 1,239 under
    // 下さる 2,574), or the kanji lemma has another class, they keep it.
    let lemmas = vec![
        lemma("でる", "出る", 680, 41),
        lemma("でる", "でる", 680, 1_269),
        lemma("つなぐ", "繋ぐ", 837, 3_000),
        lemma("つなぐ", "つなぐ", 837, 3_000),
        lemma("くださる", "下さる", 837, 2_574),
        lemma("くださる", "くださる", 837, 1_239),
        lemma("ふさぐ", "塞ぐ", 723, 1_000),
        lemma("ふさぐ", "ふさぐ", 837, 2_000),
    ];
    let (entries, report) = expand_inflections(&lemmas, &catalog()).expect("expand");
    for (reading, surface) in [
        ("でて", "でて"),
        ("でました", "でました"),
        ("つないで", "つないで"),
    ] {
        let form = pair(&entries, reading, surface);
        assert_eq!(form.flags, EntryFlags::NONE, "{reading} -> {surface}");
        assert_eq!(form.prediction_cost, i32::MAX, "{reading} -> {surface}");
    }
    assert_eq!(pair(&entries, "でて", "でて").word_cost, 1_269 + 20);
    for (reading, surface) in [
        ("でて", "出て"),
        ("つないで", "繋いで"),
        ("くださって", "くださって"),
        ("ふさいで", "ふさいで"),
    ] {
        let form = pair(&entries, reading, surface);
        assert_eq!(form.flags, EntryFlags::PREDICTION, "{reading} -> {surface}");
    }
    // Count the kana forms whose kanji form predicts: forms such as でない
    // leave prediction for every lemma (Issue #400), not for a kanji sibling.
    let unpredicted = entries
        .iter()
        .filter(|entry| entry.surface == entry.reading && entry.flags == EntryFlags::NONE)
        .filter(|entry| {
            entries.iter().any(|other| {
                other.reading == entry.reading && other.flags.contains(EntryFlags::PREDICTION)
            })
        })
        .filter(|entry| entry.reading.starts_with("で") || entry.reading.starts_with("つな"))
        .filter(|entry| entry.reading.chars().count() >= 2)
        .count();
    assert!(unpredicted > 0);
    assert_eq!(report.kana_forms_unpredicted, unpredicted);
}

#[test]
fn a_verb_predicts_only_its_te_ta_masu_and_contracted_te_forms() {
    // Issue #400: with every generated form predictable, the forms of one
    // verb (振り込んで, 振り込みます, 振り込みたい …) filled the nine and pushed
    // out other words such as 振込手数料. The other forms stay convertible.
    let lemmas = vec![lemma("かく", "書く", 723, 50)];
    let (entries, _) = expand_inflections(&lemmas, &catalog()).expect("expand");
    for (reading, surface) in [
        ("かいて", "書いて"),
        ("かいた", "書いた"),
        ("かきます", "書きます"),
        ("かいてる", "書いてる"),
        ("かいている", "書いている"),
        ("かいてない", "書いてない"),
    ] {
        let form = pair(&entries, reading, surface);
        assert_eq!(form.flags, EntryFlags::PREDICTION, "{reading} -> {surface}");
    }
    for (reading, surface) in [
        ("かいたら", "書いたら"),
        ("かかない", "書かない"),
        ("かきました", "書きました"),
        ("かきたい", "書きたい"),
        ("かけば", "書けば"),
        ("かこう", "書こう"),
    ] {
        let form = pair(&entries, reading, surface);
        assert_eq!(form.flags, EntryFlags::NONE, "{reading} -> {surface}");
        assert_eq!(form.prediction_cost, i32::MAX, "{reading} -> {surface}");
        assert!(form.word_cost < i32::MAX, "{reading} stays convertible");
    }
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
