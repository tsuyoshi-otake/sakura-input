use dictc_core::inflection::{
    expand_inflections, inherit_inflection_details, inherit_native_inflection_details,
    parse_inflection_pos_catalog,
};
use dictc_core::{
    compile, merge_entries, parse_connection, parse_entries, SourceDetail, SourceEntry,
};
use sakura_core::conversion::{ConversionOptions, Converter};
use sakura_core::dictionary::{Dictionary, EntryFlags};
use sakura_core::ConversionMethod;

const POS: &str = "\
45 助動詞,*,*,*,不変化型,基本形,ん
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
652 動詞,自立,*,*,一段,仮定縮約１,*
659 動詞,自立,*,*,一段,体言接続特殊,*
666 動詞,自立,*,*,一段,命令ｒｏ,*
673 動詞,自立,*,*,一段,命令ｙｏ,*
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
721 動詞,自立,*,*,五段・カ行イ音便,仮定縮約１,*
722 動詞,自立,*,*,五段・カ行イ音便,命令ｅ,*
731 動詞,自立,*,*,五段・カ行促音便,基本形,行く
733 動詞,自立,*,*,五段・カ行促音便,未然形,行く
732 動詞,自立,*,*,五段・カ行促音便,未然ウ接続,行く
734 動詞,自立,*,*,五段・カ行促音便,連用タ接続,行く
735 動詞,自立,*,*,五段・カ行促音便,連用形,行く
728 動詞,自立,*,*,五段・カ行促音便,仮定形,行く
730 動詞,自立,*,*,五段・カ行促音便,命令ｅ,行く
833 動詞,自立,*,*,五段動詞,仮定形,*
835 動詞,自立,*,*,五段動詞,体言接続特殊,*
836 動詞,自立,*,*,五段動詞,命令ｅ,*
837 動詞,自立,*,*,五段動詞,基本形,*
838 動詞,自立,*,*,五段動詞,未然ウ接続,*
839 動詞,自立,*,*,五段動詞,未然形,*
840 動詞,自立,*,*,五段動詞,未然特殊,*
841 動詞,自立,*,*,五段動詞,連用タ接続,*
842 動詞,自立,*,*,五段動詞,連用形,*
617 動詞,自立,*,*,サ変,仮定形,*
618 動詞,自立,*,*,サ変,仮定縮約１,*
620 動詞,自立,*,*,サ変,命令ｙｏ,*
621 動詞,自立,*,*,サ変,基本形,*
623 動詞,自立,*,*,サ変,未然ウ接続,*
624 動詞,自立,*,*,サ変,未然レル接続,*
633 動詞,自立,*,*,サ変・スル,基本形,*
637 動詞,自立,*,*,サ変・スル,未然形,*
634 動詞,自立,*,*,サ変・スル,未然ウ接続,*
638 動詞,自立,*,*,サ変・スル,連用形,*
627 動詞,自立,*,*,サ変・スル,仮定形,*
628 動詞,自立,*,*,サ変・スル,仮定縮約１,*
631 動詞,自立,*,*,サ変・スル,命令ｒｏ,*
632 動詞,自立,*,*,サ変・スル,命令ｙｏ,*
636 動詞,自立,*,*,サ変・スル,未然レル接続,*
2425 形容詞,自立,*,*,形容詞・アウオ段,基本形,*
2393 形容詞,自立,*,*,形容詞・アウオ段,ガル接続,*
1851 名詞,一般,*,*,*,*,*
2413 形容詞,自立,*,*,形容詞・アウオ段,体言接続,*
2408 形容詞,自立,*,*,形容詞・アウオ段,仮定縮約２,*
2419 形容詞,自立,*,*,形容詞・アウオ段,口語基本形-促音便,*
2444 形容詞,自立,*,*,形容詞・アウオ段,連用ゴザイ接続,*
2420 形容詞,自立,*,*,形容詞・アウオ段,命令ｅ,*
2439 形容詞,自立,*,*,形容詞・アウオ段,未然ヌ接続,*
2398 形容詞,自立,*,*,形容詞・アウオ段,仮定形,*
2403 形容詞,自立,*,*,形容詞・アウオ段,仮定縮約１,*
2434 形容詞,自立,*,*,形容詞・アウオ段,未然ウ接続,*
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

fn source_detail(entry: &SourceEntry, description: &str) -> SourceDetail {
    SourceDetail {
        reading: entry.reading.clone(),
        surface: entry.surface.clone(),
        left_id: entry.left_id,
        right_id: entry.right_id,
        description: description.to_owned(),
        relations: Vec::new(),
    }
}

#[test]
fn inflection_detail_names_its_lemma_for_negative_and_conditional_forms() {
    let lemmas = vec![lemma("くる", "来る", 610, 7)];
    let (generated, _) = expand_inflections(&lemmas, &catalog()).unwrap();
    let final_entries = merge_entries(lemmas.clone(), generated.clone()).unwrap();
    let existing = vec![source_detail(&lemmas[0], "こちらへ移動すること。")];
    let (details, report) =
        inherit_inflection_details(&lemmas, &generated, &final_entries, &existing, &catalog())
            .unwrap();
    assert!(report.emitted > 0);
    for surface in ["来て", "来ない", "来たら"] {
        let detail = details
            .iter()
            .find(|d| d.surface == surface)
            .expect(surface);
        assert_eq!(
            detail.description,
            "「来る」の活用形。基本形の意味：こちらへ移動すること。"
        );
        assert!(detail.relations.is_empty());
    }
}

#[test]
fn inflection_detail_never_overwrites_existing_or_uses_missing_lemma() {
    let lemmas = vec![lemma("くる", "来る", 610, 7)];
    let (generated, _) = expand_inflections(&lemmas, &catalog()).unwrap();
    let final_entries = merge_entries(lemmas.clone(), generated.clone()).unwrap();
    let target = pair(&generated, "きて", "来て");
    let existing = vec![
        source_detail(&lemmas[0], "基本形の説明。"),
        source_detail(target, "既存の説明。"),
    ];
    let (details, _) =
        inherit_inflection_details(&lemmas, &generated, &final_entries, &existing, &catalog())
            .unwrap();
    assert!(!details
        .iter()
        .any(|d| d.reading == "きて" && d.surface == "来て"));
    let missing = final_entries
        .iter()
        .filter(|e| e.reading != "くる" || e.surface != "来る")
        .cloned()
        .collect::<Vec<_>>();
    let (details, _) =
        inherit_inflection_details(&lemmas, &generated, &missing, &existing, &catalog()).unwrap();
    assert!(details.is_empty());
}

#[test]
fn inflection_detail_rejects_ambiguous_lemma_even_when_only_one_has_detail() {
    let catalog =
        parse_inflection_pos_catalog("id.def", &format!("{POS}681 動詞,自立,*,*,一段,基本形,*\n"))
            .unwrap();
    let lemmas = vec![
        lemma("たべる", "食べる", 680, 7),
        lemma("たべる", "食べる", 681, 8),
    ];
    let (generated, _) = expand_inflections(&lemmas, &catalog).unwrap();
    let final_entries = merge_entries(lemmas.clone(), generated.clone()).unwrap();
    let (details, report) = inherit_inflection_details(
        &lemmas,
        &generated,
        &final_entries,
        &[source_detail(&lemmas[0], "食物を口にする。")],
        &catalog,
    )
    .unwrap();
    assert!(details.is_empty());
    assert!(report.ambiguous > 0);
}

#[test]
fn inflection_detail_rejects_a_stale_generated_overlay() {
    let lemmas = vec![lemma("くる", "来る", 610, 7)];
    let (mut generated, _) = expand_inflections(&lemmas, &catalog()).unwrap();
    let final_entries = merge_entries(lemmas.clone(), generated.clone()).unwrap();
    generated[0].word_cost += 1;
    assert!(inherit_inflection_details(
        &lemmas,
        &generated,
        &final_entries,
        &[source_detail(&lemmas[0], "説明。")],
        &catalog()
    )
    .is_err());
}

#[test]
fn inflection_detail_ignores_a_final_entry_with_modified_cost() {
    let lemmas = vec![lemma("くる", "来る", 610, 7)];
    let (generated, _) = expand_inflections(&lemmas, &catalog()).unwrap();
    let mut final_entries = merge_entries(lemmas.clone(), generated.clone()).unwrap();
    let changed = final_entries
        .iter_mut()
        .find(|e| e.reading == "きて" && e.surface == "来て")
        .unwrap();
    changed.word_cost += 1;
    let (details, report) = inherit_inflection_details(
        &lemmas,
        &generated,
        &final_entries,
        &[source_detail(&lemmas[0], "説明。")],
        &catalog(),
    )
    .unwrap();
    assert!(!details
        .iter()
        .any(|d| d.reading == "きて" && d.surface == "来て"));
    assert_eq!(report.final_changed, 1);
}

#[test]
fn native_inflection_detail_inheritance_requires_one_unchanged_basic_identity() {
    let base = lemma("くる", "来る", 610, 7);
    let native = lemma("きて", "来て", 616, 40);
    let lemmas = vec![base.clone(), native.clone()];
    let final_entries = merge_entries(lemmas.clone(), Vec::new()).unwrap();
    let (details, report) = inherit_native_inflection_details(
        &lemmas,
        &final_entries,
        &[source_detail(&base, "こちらへ移動すること。")],
        &catalog(),
    )
    .unwrap();
    let inherited = details
        .iter()
        .find(|d| d.surface == "来て")
        .expect("native form meaning");
    assert_eq!(
        inherited.description,
        "「来る」の活用形。基本形の意味：こちらへ移動すること。"
    );
    assert!(inherited.relations.is_empty());
    assert_eq!(report.emitted, 1);
}

#[test]
fn native_adjective_adverbial_inherits_only_from_one_unchanged_parent() {
    let base = lemma("たかい", "高い", 2425, 7);
    let native = lemma("たかく", "高く", 2454, 40);
    let lemmas = vec![base.clone(), native.clone()];
    let (generated, _) = expand_inflections(&lemmas, &catalog()).unwrap();
    assert!(!generated.iter().any(|entry| entry.surface == "高く"));
    let final_entries = merge_entries(lemmas.clone(), generated).unwrap();
    let base_detail = source_detail(&base, "高さがある。");
    let (details, _) = inherit_native_inflection_details(
        &lemmas,
        &final_entries,
        std::slice::from_ref(&base_detail),
        &catalog(),
    )
    .unwrap();
    assert_eq!(
        details
            .iter()
            .find(|d| d.surface == "高く")
            .unwrap()
            .description,
        "「高い」の活用形。基本形の意味：高さがある。"
    );

    let mut changed = final_entries.clone();
    changed
        .iter_mut()
        .find(|entry| entry.surface == "高く")
        .unwrap()
        .word_cost += 1;
    let (details, _) = inherit_native_inflection_details(
        &lemmas,
        &changed,
        std::slice::from_ref(&base_detail),
        &catalog(),
    )
    .unwrap();
    assert!(!details.iter().any(|d| d.surface == "高く"));

    let existing = [base_detail.clone(), source_detail(&native, "既存の説明。")];
    let (details, _) =
        inherit_native_inflection_details(&lemmas, &final_entries, &existing, &catalog()).unwrap();
    assert!(!details.iter().any(|d| d.surface == "高く"));

    let duplicate_catalog = parse_inflection_pos_catalog(
        "id.def",
        &format!("{POS}2426 形容詞,自立,*,*,形容詞・アウオ段,基本形,*\n"),
    )
    .unwrap();
    let ambiguous = vec![base, lemma("たかい", "高い", 2426, 8), native];
    let final_ambiguous = merge_entries(ambiguous.clone(), Vec::new()).unwrap();
    let (details, report) = inherit_native_inflection_details(
        &ambiguous,
        &final_ambiguous,
        &[base_detail],
        &duplicate_catalog,
    )
    .unwrap();
    assert!(!details.iter().any(|d| d.surface == "高く"));
    assert_eq!(report.ambiguous, 1);
}

#[test]
fn native_godan_stems_keep_conjugation_classes_separate() {
    let base = lemma("かく", "書く", 723, 7);
    let targets = [
        ("かい", "書い", 726),
        ("かけ", "書け", 720),
        ("かこ", "書こ", 724),
    ];
    let mut lemmas = vec![base.clone()];
    lemmas.extend(
        targets
            .iter()
            .map(|(reading, surface, id)| lemma(reading, surface, *id, 40)),
    );
    let (generated, _) = expand_inflections(&lemmas, &catalog()).unwrap();
    assert!(targets.iter().all(|(reading, surface, _)| !generated
        .iter()
        .any(|e| e.reading == *reading && e.surface == *surface)));
    let final_entries = merge_entries(lemmas.clone(), generated).unwrap();
    let (details, _) = inherit_native_inflection_details(
        &lemmas,
        &final_entries,
        &[source_detail(&base, "文字を記す。")],
        &catalog(),
    )
    .unwrap();
    for (_, surface, id) in targets {
        let detail = details
            .iter()
            .find(|d| d.surface == surface && d.left_id == id)
            .unwrap();
        assert_eq!(
            detail.description,
            "「書く」の活用形。基本形の意味：文字を記す。"
        );
    }
}

#[test]
fn native_voiced_godan_stem_and_wrong_form_are_distinguished() {
    let pos_catalog = parse_inflection_pos_catalog(
        "id.def",
        &format!("{POS}900 動詞,自立,*,*,五段・ガ行,基本形,*\n901 動詞,自立,*,*,五段・ガ行,連用タ接続,*\n902 動詞,自立,*,*,五段・ガ行,仮定形,*\n"),
    ).unwrap();
    let base = lemma("およぐ", "泳ぐ", 900, 7);
    let native = lemma("およい", "泳い", 901, 40);
    let wrong_form = lemma("およい", "泳い", 902, 40);
    let lemmas = vec![base.clone(), native, wrong_form];
    let final_entries = merge_entries(lemmas.clone(), Vec::new()).unwrap();
    let (details, _) = inherit_native_inflection_details(
        &lemmas,
        &final_entries,
        &[source_detail(&base, "水中を進む。")],
        &pos_catalog,
    )
    .unwrap();
    assert!(details
        .iter()
        .any(|d| d.surface == "泳い" && d.left_id == 901));
    assert!(!details
        .iter()
        .any(|d| d.surface == "泳い" && d.left_id == 902));
}

#[test]
fn native_conditional_stems_keep_parent_and_form_identity() {
    let ichidan = lemma("たべる", "食べる", 680, 7);
    let godan = lemma("かく", "書く", 723, 7);
    let targets = [
        ("たべれ", "食べれ", 645),
        ("たべよ", "食べよ", 687),
        ("たべりゃ", "食べりゃ", 652),
        ("たべん", "食べん", 659),
        ("かきゃ", "書きゃ", 721),
    ];
    let mut lemmas = vec![
        ichidan.clone(),
        godan.clone(),
        lemma("たべよ", "食べよ", 673, 30),
        lemma("たべよ", "食べよ", 701, 30),
    ];
    let mut other_negative = lemma("たべん", "食べん", 694, 30);
    other_negative.right_id = 45;
    lemmas.push(other_negative);
    lemmas.extend(targets.iter().map(|(r, s, id)| lemma(r, s, *id, 40)));
    let (generated, _) = expand_inflections(&lemmas, &catalog()).unwrap();
    assert!(targets
        .iter()
        .all(|(r, s, _)| !generated.iter().any(|e| e.reading == *r && e.surface == *s)));
    let final_entries = merge_entries(lemmas.clone(), generated).unwrap();
    let existing = [
        source_detail(&ichidan, "食物を口に入れる。"),
        source_detail(&godan, "文字を記す。"),
    ];
    let (details, _) =
        inherit_native_inflection_details(&lemmas, &final_entries, &existing, &catalog()).unwrap();
    for (_, surface, id) in targets {
        assert!(details
            .iter()
            .any(|d| d.surface == surface && d.left_id == id));
    }
    assert!(details
        .iter()
        .any(|d| d.surface == "食べよ" && d.left_id == 673));
    assert!(!details
        .iter()
        .any(|d| d.surface == "食べよ" && d.left_id == 701));
    assert!(details
        .iter()
        .any(|d| d.surface == "食べん" && d.left_id == 694 && d.right_id == 45));
}

#[test]
fn native_sahen_stems_inherit_only_the_matching_form() {
    let base = lemma("あいする", "愛する", 621, 7);
    let other_base = lemma("かんする", "関する", 633, 7);
    let targets = [
        ("あいすれ", "愛すれ", 617),
        ("あいすりゃ", "愛すりゃ", 618),
        ("あいしよ", "愛しよ", 623),
        ("あいせ", "愛せ", 624),
        ("あいせよ", "愛せよ", 620),
        ("かんすれ", "関すれ", 627),
        ("かんすりゃ", "関すりゃ", 628),
        ("かんしよ", "関しよ", 634),
        ("かんせ", "関せ", 636),
        ("かんせよ", "関せよ", 632),
    ];
    let mut lemmas = vec![
        base.clone(),
        other_base.clone(),
        lemma("あいすれ", "愛すれ", 1851, 40),
        lemma("あいすれ", "愛すれ", 627, 40),
    ];
    let mut fused = lemma("あいせよ", "愛せよ", 620, 40);
    fused.right_id = 1851;
    lemmas.push(fused);
    let mut other_fused = lemma("かんせ", "関せ", 636, 40);
    other_fused.right_id = 1851;
    lemmas.push(other_fused);
    lemmas.extend(targets.iter().map(|(r, s, id)| lemma(r, s, *id, 40)));
    let (generated, _) = expand_inflections(&lemmas, &catalog()).unwrap();
    assert!(targets.iter().all(|(r, s, _)| !generated
        .iter()
        .any(|entry| entry.reading == *r && entry.surface == *s)));
    let final_entries = merge_entries(lemmas.clone(), generated).unwrap();
    let (details, _) = inherit_native_inflection_details(
        &lemmas,
        &final_entries,
        &[
            source_detail(&base, "大切に思う。"),
            source_detail(&other_base, "関係がある。"),
        ],
        &catalog(),
    )
    .unwrap();
    for (_, surface, id) in targets {
        assert!(details
            .iter()
            .any(|d| d.surface == surface && d.left_id == id));
    }
    assert!(!details
        .iter()
        .any(|d| d.surface == "愛すれ" && d.left_id == 1851));
    assert!(!details
        .iter()
        .any(|d| d.surface == "愛すれ" && d.left_id == 627));
    assert!(!details
        .iter()
        .any(|d| d.surface == "愛せよ" && d.right_id == 1851));
    assert!(!details
        .iter()
        .any(|d| d.surface == "関せ" && d.right_id == 1851));
    let native = lemmas
        .iter()
        .find(|e| e.surface == "愛すれ" && e.left_id == 617)
        .unwrap();
    let mut changed = final_entries.clone();
    changed
        .iter_mut()
        .find(|e| e.surface == "愛すれ" && e.left_id == 617)
        .unwrap()
        .word_cost += 1;
    let (details, _) = inherit_native_inflection_details(
        &lemmas,
        &changed,
        &[source_detail(&base, "大切に思う。")],
        &catalog(),
    )
    .unwrap();
    assert!(!details
        .iter()
        .any(|d| d.surface == "愛すれ" && d.left_id == 617));
    let (details, _) = inherit_native_inflection_details(
        &lemmas,
        &final_entries,
        &[
            source_detail(&base, "大切に思う。"),
            source_detail(native, "既存の説明。"),
        ],
        &catalog(),
    )
    .unwrap();
    assert!(!details
        .iter()
        .any(|d| d.surface == "愛すれ" && d.left_id == 617));
    let pos = format!("{POS}1900 動詞,自立,*,*,サ変,基本形,*\n");
    let ambiguous_catalog = parse_inflection_pos_catalog("id.def", &pos).unwrap();
    let mut ambiguous = lemmas.clone();
    ambiguous.push(lemma("あいする", "愛する", 1900, 8));
    let ambiguous_final = merge_entries(ambiguous.clone(), Vec::new()).unwrap();
    let (details, _) = inherit_native_inflection_details(
        &ambiguous,
        &ambiguous_final,
        &[source_detail(&base, "大切に思う。")],
        &ambiguous_catalog,
    )
    .unwrap();
    assert!(!details
        .iter()
        .any(|d| d.surface == "愛すれ" && d.left_id == 617));
}

#[test]
fn native_negative_requires_the_exact_auxiliary_identity() {
    let base = lemma("かく", "書く", 723, 7);
    let mut native = lemma("かかん", "書かん", 725, 40);
    native.right_id = 45;
    let other = lemma("かかん", "書かん", 725, 41);
    let lemmas = vec![base.clone(), native, other];
    let entries = merge_entries(lemmas.clone(), Vec::new()).unwrap();
    let (details, _) = inherit_native_inflection_details(
        &lemmas,
        &entries,
        &[source_detail(&base, "文字を記す。")],
        &catalog(),
    )
    .unwrap();
    assert!(details
        .iter()
        .any(|d| d.surface == "書かん" && d.right_id == 45));
    assert!(!details
        .iter()
        .any(|d| d.surface == "書かん" && d.right_id == 725));
    let without_aux = parse_inflection_pos_catalog(
        "id.def",
        &POS.replace("45 助動詞,*,*,*,不変化型,基本形,ん\n", ""),
    )
    .unwrap();
    let (details, _) = inherit_native_inflection_details(
        &lemmas,
        &entries,
        &[source_detail(&base, "文字を記す。")],
        &without_aux,
    )
    .unwrap();
    assert!(!details.iter().any(|d| d.surface == "書かん"));
}

#[test]
fn native_stems_require_their_own_pos_and_form_class() {
    let adjective = lemma("たかい", "高い", 2425, 7);
    let verb = lemma("あつまる", "集まる", 837, 7);
    let targets = [
        ("たか", "高", 2393),
        ("あつまん", "集まん", 835),
        ("あつまん", "集まん", 840),
    ];
    let mut lemmas = vec![
        adjective.clone(),
        verb.clone(),
        lemma("たか", "高", 1851, 30),
    ];
    lemmas.extend(targets.iter().map(|(r, s, id)| lemma(r, s, *id, 40)));
    let (generated, _) = expand_inflections(&lemmas, &catalog()).unwrap();
    assert!(targets
        .iter()
        .all(|(r, s, _)| !generated.iter().any(|e| e.reading == *r && e.surface == *s)));
    let final_entries = merge_entries(lemmas.clone(), generated).unwrap();
    let existing = [
        source_detail(&adjective, "高さがある。"),
        source_detail(&verb, "人や物が一か所に寄る。"),
    ];
    let (details, _) =
        inherit_native_inflection_details(&lemmas, &final_entries, &existing, &catalog()).unwrap();
    for (_, surface, id) in targets {
        assert!(details
            .iter()
            .any(|d| d.surface == surface && d.left_id == id));
    }
    assert!(!details
        .iter()
        .any(|d| d.surface == "高" && d.left_id == 1851));
}

#[test]
fn native_adjective_stems_inherit_without_generating_candidates() {
    let base = lemma("たかい", "高い", 2425, 7);
    let targets = [
        ("たかかっ", "高かっ", 2449),
        ("たかかろ", "高かろ", 2434),
        ("たかけれ", "高けれ", 2398),
        ("たかけりゃ", "高けりゃ", 2403),
        ("たかき", "高き", 2413),
        ("たかから", "高から", 2439),
        ("たかかれ", "高かれ", 2420),
        ("たかきゃ", "高きゃ", 2408),
        ("たかう", "高う", 2444),
        ("たかぅ", "高ぅ", 2444),
        ("たかっ", "高っ", 2419),
    ];
    let mut lemmas = vec![base.clone()];
    lemmas.extend(
        targets
            .iter()
            .map(|(reading, surface, id)| lemma(reading, surface, *id, 40)),
    );
    let (generated, _) = expand_inflections(&lemmas, &catalog()).unwrap();
    assert!(targets
        .iter()
        .all(|(_, surface, _)| !generated.iter().any(|e| e.surface == *surface)));
    let final_entries = merge_entries(lemmas.clone(), generated).unwrap();
    let (details, _) = inherit_native_inflection_details(
        &lemmas,
        &final_entries,
        &[source_detail(&base, "高さがある。")],
        &catalog(),
    )
    .unwrap();
    for (_, surface, _) in targets {
        assert_eq!(
            details
                .iter()
                .find(|d| d.surface == surface)
                .unwrap()
                .description,
            "「高い」の活用形。基本形の意味：高さがある。"
        );
    }
}

#[test]
fn native_inflection_detail_rejects_ambiguous_changed_and_preannotated_entries() {
    let pos_catalog = parse_inflection_pos_catalog(
        "id.def",
        &format!("{POS}681 動詞,自立,*,*,カ変・来ル,基本形,来る\n"),
    )
    .unwrap();
    let base = lemma("くる", "来る", 610, 7);
    let other = lemma("くる", "来る", 681, 8);
    let native = lemma("きて", "来て", 616, 40);
    let lemmas = vec![base.clone(), other, native.clone()];
    let mut final_entries = merge_entries(lemmas.clone(), Vec::new()).unwrap();
    let key = final_entries
        .iter_mut()
        .find(|entry| entry.surface == "来て")
        .unwrap();
    key.word_cost += 1;
    let existing = [
        source_detail(&base, "基本形の説明。"),
        source_detail(&native, "既存の説明。"),
    ];
    let (details, report) =
        inherit_native_inflection_details(&lemmas, &final_entries, &existing, &pos_catalog)
            .unwrap();
    assert!(details.is_empty());
    assert!(report.ambiguous > 0);
    assert_eq!(report.final_changed, 0);

    let single = vec![base.clone(), native.clone()];
    let mut changed_final = merge_entries(single.clone(), Vec::new()).unwrap();
    changed_final
        .iter_mut()
        .find(|entry| entry.surface == "来て")
        .unwrap()
        .word_cost += 1;
    let (details, report) = inherit_native_inflection_details(
        &single,
        &changed_final,
        &[source_detail(&base, "意味。")],
        &catalog(),
    )
    .unwrap();
    assert!(details.is_empty());
    assert_eq!(report.final_changed, 1);

    let (details, report) = inherit_native_inflection_details(
        &single,
        &merge_entries(single.clone(), Vec::new()).unwrap(),
        &existing,
        &catalog(),
    )
    .unwrap();
    assert!(details.is_empty());
    assert_eq!(report.existing, 1);
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
