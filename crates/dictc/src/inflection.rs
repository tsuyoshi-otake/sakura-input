//! Deterministic expansion of Japanese conjugations for the system lexicon.
//!
//! Sakura stores static lattice edges and does not inflect at runtime. Mozc's
//! trimmed shards keep dictionary-form verbs and some colloquial compounds, but
//! drop everyday fused forms such as `来て`. This module reconstructs those
//! forms from 基本形 lemmas using the pinned Mozc `id.def` taxonomy so each new
//! edge keeps a real connection class.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use sakura_core::dictionary::EntryFlags;

use crate::SourceEntry;

const PREDICTION_OFFSET: i32 = 1_200;
const PREDICTION_COST_LIMIT: i32 = 6_000;

/// One row of Mozc `id.def`, keeping empty `*` slots so conjugation type and
/// form stay aligned.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PosRow {
    id: u16,
    pos: String,
    subpos: String,
    ctype: String,
    cform: String,
    lemma: String,
}

#[derive(Debug, Default)]
pub struct InflectionPosCatalog {
    by_id: BTreeMap<u16, PosRow>,
    by_key: BTreeMap<(String, String, String, String, String), u16>,
}

impl InflectionPosCatalog {
    fn row(&self, id: u16) -> Option<&PosRow> {
        self.by_id.get(&id)
    }

    fn lookup(
        &self,
        pos: &str,
        subpos: &str,
        ctype: &str,
        cform: &str,
        lemma: &str,
    ) -> Option<u16> {
        if lemma != "*" {
            if let Some(id) = self
                .by_key
                .get(&(
                    pos.to_string(),
                    subpos.to_string(),
                    ctype.to_string(),
                    cform.to_string(),
                    lemma.to_string(),
                ))
                .copied()
            {
                return Some(id);
            }
        }
        self.by_key
            .get(&(
                pos.to_string(),
                subpos.to_string(),
                ctype.to_string(),
                cform.to_string(),
                "*".to_string(),
            ))
            .copied()
    }
}

/// Parses Mozc `id.def` without dropping `*` fields.
pub fn parse_inflection_pos_catalog(
    source: &str,
    text: &str,
) -> Result<InflectionPosCatalog, String> {
    let mut catalog = InflectionPosCatalog::default();
    for (zero_based, raw) in text.lines().enumerate() {
        let line_number = zero_based + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (id, definition) = line
            .split_once(char::is_whitespace)
            .ok_or_else(|| format!("{source}:{line_number}: expected '<id> <POS definition>'"))?;
        let id = id
            .parse::<u16>()
            .map_err(|_| format!("{source}:{line_number}: invalid POS id '{id}'"))?;
        let fields: Vec<&str> = definition.split(',').map(str::trim).collect();
        if fields.len() < 6 {
            return Err(format!(
                "{source}:{line_number}: POS definition needs at least 6 comma-separated fields"
            ));
        }
        let row = PosRow {
            id,
            pos: fields[0].to_string(),
            subpos: fields[1].to_string(),
            ctype: fields[4].to_string(),
            cform: fields[5].to_string(),
            lemma: fields.get(6).copied().unwrap_or("*").to_string(),
        };
        let key = (
            row.pos.clone(),
            row.subpos.clone(),
            row.ctype.clone(),
            row.cform.clone(),
            row.lemma.clone(),
        );
        if catalog.by_id.insert(id, row).is_some() {
            return Err(format!("{source}:{line_number}: duplicate POS id {id}"));
        }
        catalog.by_key.entry(key).or_insert(id);
    }
    if catalog.by_id.is_empty() {
        return Err(format!("{source}: no POS definitions found"));
    }
    Ok(catalog)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InflectionReport {
    pub lemma_entries: usize,
    pub emitted_entries: usize,
    pub skipped_existing: usize,
    pub skipped_unsupported: usize,
}

#[derive(Clone, Copy)]
struct FormSpec {
    cost_delta: i32,
    cforms: &'static [&'static str],
}

const VERB_TE: FormSpec = FormSpec {
    cost_delta: 20,
    cforms: &["連用タ接続", "連用形"],
};
const VERB_TA: FormSpec = FormSpec {
    cost_delta: 25,
    cforms: &["連用タ接続", "連用形"],
};
const VERB_TARA: FormSpec = FormSpec {
    cost_delta: 30,
    cforms: &["連用タ接続", "連用形"],
};
const VERB_NAI: FormSpec = FormSpec {
    cost_delta: 30,
    cforms: &["未然形"],
};
const VERB_NAKATTA: FormSpec = FormSpec {
    cost_delta: 40,
    cforms: &["未然形"],
};
const VERB_MASU: FormSpec = FormSpec {
    cost_delta: 40,
    cforms: &["連用形"],
};
const VERB_MASHITA: FormSpec = FormSpec {
    cost_delta: 50,
    cforms: &["連用形"],
};
const VERB_TAI: FormSpec = FormSpec {
    cost_delta: 45,
    cforms: &["連用形"],
};
const VERB_BA: FormSpec = FormSpec {
    cost_delta: 35,
    cforms: &["仮定形"],
};
const VERB_VOLITIONAL: FormSpec = FormSpec {
    cost_delta: 40,
    cforms: &["未然ウ接続"],
};
const VERB_IMPERATIVE: FormSpec = FormSpec {
    cost_delta: 60,
    cforms: &["命令ｉ", "命令ｒｏ", "命令ｅ", "命令ｙｏ"],
};
const VERB_RENYOU: FormSpec = FormSpec {
    cost_delta: 15,
    cforms: &["連用形"],
};
const VERB_MIZEN: FormSpec = FormSpec {
    cost_delta: 20,
    cforms: &["未然形"],
};
const VERB_TERU: FormSpec = FormSpec {
    cost_delta: 35,
    cforms: &["連用タ接続", "連用形"],
};
const VERB_TEIRU: FormSpec = FormSpec {
    cost_delta: 40,
    cforms: &["連用タ接続", "連用形"],
};
const VERB_TENAI: FormSpec = FormSpec {
    cost_delta: 45,
    cforms: &["連用タ接続", "連用形"],
};
const ADJ_TE: FormSpec = FormSpec {
    cost_delta: 20,
    cforms: &["連用テ接続"],
};
const ADJ_TA: FormSpec = FormSpec {
    cost_delta: 25,
    cforms: &["連用タ接続"],
};
const ADJ_NAI: FormSpec = FormSpec {
    cost_delta: 30,
    cforms: &["連用テ接続", "未然ヌ接続"],
};
const ADJ_NAKATTA: FormSpec = FormSpec {
    cost_delta: 40,
    cforms: &["連用タ接続"],
};
const ADJ_BA: FormSpec = FormSpec {
    cost_delta: 35,
    cforms: &["仮定形"],
};

/// Expands 基本形 verbs and i-adjectives into fused IME forms.
///
/// Existing `(reading, surface)` pairs are left untouched. Connection ids come
/// from the lemma's Mozc POS family; a form is skipped rather than invented
/// when that family has no matching 活用形 id.
pub fn expand_inflections(
    lemmas: &[SourceEntry],
    catalog: &InflectionPosCatalog,
) -> Result<(Vec<SourceEntry>, InflectionReport), String> {
    let existing: BTreeSet<(String, String)> = lemmas
        .iter()
        .map(|entry| (entry.reading.clone(), entry.surface.clone()))
        .collect();
    let mut emitted = Vec::new();
    let mut seen = existing.clone();
    let mut report = InflectionReport::default();
    let source: Arc<str> = Arc::from("inflection-expand");

    for lemma in lemmas {
        let Some(row) = catalog.row(lemma.left_id) else {
            report.skipped_unsupported = report.skipped_unsupported.saturating_add(1);
            continue;
        };
        if row.cform != "基本形" {
            continue;
        }
        if row.subpos == "接尾" {
            continue;
        }
        report.lemma_entries = report.lemma_entries.saturating_add(1);
        let forms = match conjugate(row, lemma) {
            Some(forms) if !forms.is_empty() => forms,
            Some(_) | None => {
                report.skipped_unsupported = report.skipped_unsupported.saturating_add(1);
                continue;
            }
        };
        for form in forms {
            if !seen.insert((form.reading.clone(), form.surface.clone())) {
                report.skipped_existing = report.skipped_existing.saturating_add(1);
                continue;
            }
            let Some(class_id) = resolve_form_id(catalog, row, form.spec.cforms) else {
                report.skipped_unsupported = report.skipped_unsupported.saturating_add(1);
                continue;
            };
            let word_cost = lemma.word_cost.saturating_add(form.spec.cost_delta);
            let prediction_worthy =
                form.reading.chars().count() >= 2 && word_cost <= PREDICTION_COST_LIMIT;
            emitted.push(SourceEntry::derived(
                Arc::clone(&source),
                emitted.len().saturating_add(1),
                form.reading,
                form.surface,
                class_id,
                class_id,
                word_cost,
                if prediction_worthy {
                    word_cost.saturating_add(PREDICTION_OFFSET)
                } else {
                    i32::MAX
                },
                if prediction_worthy {
                    EntryFlags::PREDICTION
                } else {
                    EntryFlags::NONE
                },
            ));
        }
    }

    emitted.sort_by(|left, right| {
        (
            &left.reading,
            &left.surface,
            left.left_id,
            left.right_id,
            left.word_cost,
        )
            .cmp(&(
                &right.reading,
                &right.surface,
                right.left_id,
                right.right_id,
                right.word_cost,
            ))
    });
    report.emitted_entries = emitted.len();
    Ok((emitted, report))
}

struct ConjugatedForm {
    spec: FormSpec,
    reading: String,
    surface: String,
}

fn conjugate(row: &PosRow, lemma: &SourceEntry) -> Option<Vec<ConjugatedForm>> {
    if row.pos == "動詞" {
        conjugate_verb(row, lemma)
    } else if row.pos == "形容詞" {
        conjugate_adjective(row, lemma)
    } else {
        None
    }
}

fn conjugate_verb(row: &PosRow, lemma: &SourceEntry) -> Option<Vec<ConjugatedForm>> {
    if irregular_nai_lemma(&lemma.surface, &lemma.reading) {
        let te = replace_mora_ending(&lemma.reading, &lemma.surface, "る", "って")?;
        return Some(vec![
            form(VERB_TE, te.clone()),
            form(
                VERB_TA,
                replace_mora_ending(&lemma.reading, &lemma.surface, "る", "った")?,
            ),
            form(
                VERB_MASU,
                replace_mora_ending(&lemma.reading, &lemma.surface, "る", "ります")?,
            ),
        ]);
    }

    let endings = verb_endings(&row.ctype)?;
    let mut forms = Vec::new();
    push_replaced(&mut forms, lemma, VERB_TE, &endings.dict, &endings.te);
    push_replaced(&mut forms, lemma, VERB_TA, &endings.dict, &endings.ta);
    push_replaced(&mut forms, lemma, VERB_TARA, &endings.dict, &endings.tara);
    push_replaced(&mut forms, lemma, VERB_NAI, &endings.dict, &endings.nai);
    push_replaced(
        &mut forms,
        lemma,
        VERB_NAKATTA,
        &endings.dict,
        &endings.nakatta,
    );
    push_replaced(&mut forms, lemma, VERB_MASU, &endings.dict, &endings.masu);
    push_replaced(
        &mut forms,
        lemma,
        VERB_MASHITA,
        &endings.dict,
        &endings.mashita,
    );
    push_replaced(&mut forms, lemma, VERB_TAI, &endings.dict, &endings.tai);
    push_replaced(&mut forms, lemma, VERB_BA, &endings.dict, &endings.ba);
    push_replaced(
        &mut forms,
        lemma,
        VERB_VOLITIONAL,
        &endings.dict,
        &endings.volitional,
    );
    push_replaced(
        &mut forms,
        lemma,
        VERB_IMPERATIVE,
        &endings.dict,
        &endings.imperative,
    );
    push_replaced(
        &mut forms,
        lemma,
        VERB_RENYOU,
        &endings.dict,
        &endings.renyou,
    );
    push_replaced(&mut forms, lemma, VERB_MIZEN, &endings.dict, &endings.mizen);
    push_replaced(&mut forms, lemma, VERB_TERU, &endings.dict, &endings.teru);
    push_replaced(&mut forms, lemma, VERB_TEIRU, &endings.dict, &endings.teiru);
    push_replaced(&mut forms, lemma, VERB_TENAI, &endings.dict, &endings.tenai);
    if forms.is_empty() {
        None
    } else {
        Some(forms)
    }
}

fn conjugate_adjective(row: &PosRow, lemma: &SourceEntry) -> Option<Vec<ConjugatedForm>> {
    if !row.ctype.starts_with("形容詞") {
        return None;
    }
    let mut forms = Vec::new();
    push_replaced(&mut forms, lemma, ADJ_TE, "い", "くて");
    push_replaced(&mut forms, lemma, ADJ_TA, "い", "かった");
    push_replaced(&mut forms, lemma, ADJ_NAI, "い", "くない");
    push_replaced(&mut forms, lemma, ADJ_NAKATTA, "い", "くなかった");
    push_replaced(&mut forms, lemma, ADJ_BA, "い", "ければ");
    if forms.is_empty() {
        None
    } else {
        Some(forms)
    }
}

struct VerbEndings {
    dict: String,
    te: String,
    ta: String,
    tara: String,
    nai: String,
    nakatta: String,
    masu: String,
    mashita: String,
    tai: String,
    ba: String,
    volitional: String,
    imperative: String,
    renyou: String,
    mizen: String,
    teru: String,
    teiru: String,
    tenai: String,
}

fn verb_endings(ctype: &str) -> Option<VerbEndings> {
    Some(match ctype {
        "カ変・クル" | "カ変・来ル" => literal_endings(
            "くる",
            "きて",
            "きた",
            "きたら",
            "こない",
            "こなかった",
            "きます",
            "きました",
            "きたい",
            "くれば",
            "こよう",
            "こい",
            "き",
            "こ",
            "きてる",
            "きている",
            "きてない",
        ),
        "サ変" | "サ変・スル" => literal_endings(
            "する",
            "して",
            "した",
            "したら",
            "しない",
            "しなかった",
            "します",
            "しました",
            "したい",
            "すれば",
            "しよう",
            "しろ",
            "し",
            "し",
            "してる",
            "している",
            "してない",
        ),
        "一段" | "一段・クレル" | "一段・得ル" => literal_endings(
            "る",
            "て",
            "た",
            "たら",
            "ない",
            "なかった",
            "ます",
            "ました",
            "たい",
            "れば",
            "よう",
            "ろ",
            "",
            "",
            "てる",
            "ている",
            "てない",
        ),
        "五段・カ行イ音便" => godan("く", "いて", "いた", "か", "き", "け", "こ"),
        "五段・カ行促音便" | "五段・カ行促音便ユク" => {
            godan("く", "って", "った", "か", "き", "け", "こ")
        }
        "五段・ガ行" => godan("ぐ", "いで", "いだ", "が", "ぎ", "げ", "ご"),
        "五段・サ行" => godan("す", "して", "した", "さ", "し", "せ", "そ"),
        "五段・タ行" => godan("つ", "って", "った", "た", "ち", "て", "と"),
        "五段・ナ行" => godan("ぬ", "んで", "んだ", "な", "に", "ね", "の"),
        "五段・バ行" => godan("ぶ", "んで", "んだ", "ば", "び", "べ", "ぼ"),
        "五段・マ行" => godan("む", "んで", "んだ", "ま", "み", "め", "も"),
        "五段・ラ行" | "五段動詞" | "五段・ラ行アル" => {
            godan("る", "って", "った", "ら", "り", "れ", "ろ")
        }
        "五段・ラ行特殊" => literal_endings(
            "る",
            "って",
            "った",
            "ったら",
            "らない",
            "らなかった",
            "います",
            "いました",
            "りたい",
            "れば",
            "ろう",
            "い",
            "り",
            "ら",
            "ってる",
            "っている",
            "ってない",
        ),
        "五段・ワ行促音便" => godan("う", "って", "った", "わ", "い", "え", "お"),
        "五段・ワ行ウ音便" => godan("う", "うて", "うた", "わ", "い", "え", "お"),
        _ => return None,
    })
}

#[allow(clippy::too_many_arguments)]
fn literal_endings(
    dict: &str,
    te: &str,
    ta: &str,
    tara: &str,
    nai: &str,
    nakatta: &str,
    masu: &str,
    mashita: &str,
    tai: &str,
    ba: &str,
    volitional: &str,
    imperative: &str,
    renyou: &str,
    mizen: &str,
    teru: &str,
    teiru: &str,
    tenai: &str,
) -> VerbEndings {
    VerbEndings {
        dict: dict.to_string(),
        te: te.to_string(),
        ta: ta.to_string(),
        tara: tara.to_string(),
        nai: nai.to_string(),
        nakatta: nakatta.to_string(),
        masu: masu.to_string(),
        mashita: mashita.to_string(),
        tai: tai.to_string(),
        ba: ba.to_string(),
        volitional: volitional.to_string(),
        imperative: imperative.to_string(),
        renyou: renyou.to_string(),
        mizen: mizen.to_string(),
        teru: teru.to_string(),
        teiru: teiru.to_string(),
        tenai: tenai.to_string(),
    }
}

fn godan(
    dict: &str,
    te: &str,
    ta: &str,
    mizen: &str,
    renyou: &str,
    e_row: &str,
    o_row: &str,
) -> VerbEndings {
    VerbEndings {
        dict: dict.to_string(),
        te: te.to_string(),
        ta: ta.to_string(),
        tara: format!("{ta}ら"),
        nai: format!("{mizen}ない"),
        nakatta: format!("{mizen}なかった"),
        masu: format!("{renyou}ます"),
        mashita: format!("{renyou}ました"),
        tai: format!("{renyou}たい"),
        ba: format!("{e_row}ば"),
        volitional: format!("{o_row}う"),
        imperative: e_row.to_string(),
        renyou: renyou.to_string(),
        mizen: mizen.to_string(),
        teru: format!("{te}る"),
        teiru: format!("{te}いる"),
        tenai: format!("{te}ない"),
    }
}

fn irregular_nai_lemma(surface: &str, reading: &str) -> bool {
    matches!(
        (surface, reading),
        ("ある", "ある") | ("有る", "ある") | ("在る", "ある")
    )
}

fn push_replaced(
    forms: &mut Vec<ConjugatedForm>,
    lemma: &SourceEntry,
    spec: FormSpec,
    from: &str,
    to: &str,
) {
    if let Some(pair) = replace_mora_ending(&lemma.reading, &lemma.surface, from, to) {
        forms.push(form(spec, pair));
    }
}

fn form(spec: FormSpec, pair: (String, String)) -> ConjugatedForm {
    ConjugatedForm {
        spec,
        reading: pair.0,
        surface: pair.1,
    }
}

fn resolve_form_id(catalog: &InflectionPosCatalog, lemma: &PosRow, cforms: &[&str]) -> Option<u16> {
    for cform in cforms {
        if let Some(id) =
            catalog.lookup(&lemma.pos, &lemma.subpos, &lemma.ctype, cform, &lemma.lemma)
        {
            return Some(id);
        }
    }
    None
}

fn replace_mora_ending(
    reading: &str,
    surface: &str,
    read_from: &str,
    read_to: &str,
) -> Option<(String, String)> {
    if read_from.is_empty() {
        if !reading.ends_with('る') {
            return None;
        }
        return replace_mora_ending(reading, surface, "る", read_to);
    }
    if !reading.ends_with(read_from) {
        return None;
    }
    let new_reading = format!("{}{read_to}", &reading[..reading.len() - read_from.len()]);
    let new_surface = replace_surface(surface, read_from, read_to)?;
    if new_reading.is_empty() || new_surface.is_empty() {
        return None;
    }
    Some((new_reading, new_surface))
}

fn replace_surface(surface: &str, read_from: &str, read_to: &str) -> Option<String> {
    if surface.chars().all(is_hiragana) && surface.ends_with(read_from) {
        return Some(format!(
            "{}{read_to}",
            &surface[..surface.len() - read_from.len()]
        ));
    }
    let trailing = trailing_hiragana(surface);
    let okuri = if trailing.ends_with(read_from) {
        read_from
    } else {
        trailing
    };
    if okuri.len() > surface.len() {
        return None;
    }
    let stem_surface = &surface[..surface.len() - okuri.len()];
    if stem_surface.is_empty() {
        return Some(read_to.to_string());
    }
    let from_morae = morae(read_from);
    let to_morae = morae(read_to);
    let okuri_morae = morae(okuri);
    if from_morae.len() < okuri_morae.len() {
        return None;
    }
    let kanji_morae = from_morae.len() - okuri_morae.len();
    if to_morae.len() < kanji_morae {
        return Some(stem_surface.to_string());
    }
    Some(format!(
        "{stem_surface}{}",
        to_morae[kanji_morae..].concat()
    ))
}

fn trailing_hiragana(text: &str) -> &str {
    let mut start = text.len();
    for (index, character) in text.char_indices().rev() {
        if is_hiragana(character) {
            start = index;
        } else {
            break;
        }
    }
    &text[start..]
}

fn is_hiragana(character: char) -> bool {
    matches!(character, '\u{3041}'..='\u{3096}' | 'ー')
}

fn morae(text: &str) -> Vec<String> {
    let mut units: Vec<String> = Vec::new();
    for character in text.chars() {
        if matches!(
            character,
            'ゃ' | 'ゅ' | 'ょ' | 'ぁ' | 'ぃ' | 'ぅ' | 'ぇ' | 'ぉ'
        ) {
            if let Some(previous) = units.last_mut() {
                previous.push(character);
                continue;
            }
        }
        units.push(character.to_string());
    }
    units
}

#[cfg(test)]
mod tests {
    use super::{parse_inflection_pos_catalog, replace_mora_ending};

    #[test]
    fn kuru_te_keeps_kanji_stem() {
        let (reading, surface) =
            replace_mora_ending("くる", "来る", "くる", "きて").expect("来るて形");
        assert_eq!(reading, "きて");
        assert_eq!(surface, "来て");
    }

    #[test]
    fn kaku_te_keeps_kanji_stem() {
        let (reading, surface) =
            replace_mora_ending("かく", "書く", "く", "いて").expect("書くて形");
        assert_eq!(reading, "かいて");
        assert_eq!(surface, "書いて");
    }

    #[test]
    fn pos_catalog_keeps_kuru_renyou() {
        let catalog = parse_inflection_pos_catalog(
            "id.def",
            "610 動詞,自立,*,*,カ変・来ル,基本形,来る\n616 動詞,自立,*,*,カ変・来ル,連用形,来る\n",
        )
        .expect("catalog");
        assert_eq!(
            catalog.lookup("動詞", "自立", "カ変・来ル", "連用形", "来る"),
            Some(616)
        );
    }
}
