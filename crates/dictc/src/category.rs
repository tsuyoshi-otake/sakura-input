//! The fixed, user-facing categories for Sakura's compiled system lexicon.
//!
//! Category files are deliberately named for what they contain.  They do not
//! encode an upstream source, acquisition method, or build layer.

use std::collections::{BTreeMap, BTreeSet};

use sakura_core::dictionary::EntryFlags;

use crate::SourceEntry;

/// One of the fourteen stable, user-facing system dictionary categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum DictionaryCategory {
    GrammarFunction = 1,
    Inflectional = 2,
    GeneralLexicon = 3,
    FixedExpressions = 4,
    NumericTimeUnits = 5,
    PersonNames = 6,
    PlaceNames = 7,
    OrganizationsProducts = 8,
    KatakanaLoanwords = 9,
    AbbreviationsAscii = 10,
    ItEngineering = 11,
    SpecialistDomains = 12,
    SymbolsEmoji = 13,
    OrthographyVariants = 14,
}

impl DictionaryCategory {
    pub const ALL: [Self; 14] = [
        Self::GrammarFunction,
        Self::Inflectional,
        Self::GeneralLexicon,
        Self::FixedExpressions,
        Self::NumericTimeUnits,
        Self::PersonNames,
        Self::PlaceNames,
        Self::OrganizationsProducts,
        Self::KatakanaLoanwords,
        Self::AbbreviationsAscii,
        Self::ItEngineering,
        Self::SpecialistDomains,
        Self::SymbolsEmoji,
        Self::OrthographyVariants,
    ];

    pub const fn id(self) -> u8 {
        self as u8
    }

    pub const fn from_id(id: u8) -> Option<Self> {
        match id {
            1 => Some(Self::GrammarFunction),
            2 => Some(Self::Inflectional),
            3 => Some(Self::GeneralLexicon),
            4 => Some(Self::FixedExpressions),
            5 => Some(Self::NumericTimeUnits),
            6 => Some(Self::PersonNames),
            7 => Some(Self::PlaceNames),
            8 => Some(Self::OrganizationsProducts),
            9 => Some(Self::KatakanaLoanwords),
            10 => Some(Self::AbbreviationsAscii),
            11 => Some(Self::ItEngineering),
            12 => Some(Self::SpecialistDomains),
            13 => Some(Self::SymbolsEmoji),
            14 => Some(Self::OrthographyVariants),
            _ => None,
        }
    }

    /// File name intentionally contains only a stable ordinal and a clear
    /// Japanese description of the contents.
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::GrammarFunction => "01_文法・機能語.tsv",
            Self::Inflectional => "02_活用語.tsv",
            Self::GeneralLexicon => "03_一般語.tsv",
            Self::FixedExpressions => "04_慣用句・定型表現.tsv",
            Self::NumericTimeUnits => "05_数値・日付・単位.tsv",
            Self::PersonNames => "06_人名.tsv",
            Self::PlaceNames => "07_地名.tsv",
            Self::OrganizationsProducts => "08_組織名・製品名.tsv",
            Self::KatakanaLoanwords => "09_外来語・カタカナ語.tsv",
            Self::AbbreviationsAscii => "10_略語・英数字.tsv",
            Self::ItEngineering => "11_IT・技術用語.tsv",
            Self::SpecialistDomains => "12_専門用語.tsv",
            Self::SymbolsEmoji => "13_記号・絵文字.tsv",
            Self::OrthographyVariants => "14_表記ゆれ.tsv",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::GrammarFunction => "文法・機能語",
            Self::Inflectional => "活用語",
            Self::GeneralLexicon => "一般語",
            Self::FixedExpressions => "慣用句・定型表現",
            Self::NumericTimeUnits => "数値・日付・単位",
            Self::PersonNames => "人名",
            Self::PlaceNames => "地名",
            Self::OrganizationsProducts => "組織名・製品名",
            Self::KatakanaLoanwords => "外来語・カタカナ語",
            Self::AbbreviationsAscii => "略語・英数字",
            Self::ItEngineering => "IT・技術用語",
            Self::SpecialistDomains => "専門用語",
            Self::SymbolsEmoji => "記号・絵文字",
            Self::OrthographyVariants => "表記ゆれ",
        }
    }
}

/// POS labels parsed from Mozc's pinned `id.def` file.
#[derive(Debug, Default)]
pub struct MozcPosCatalog {
    labels_by_id: BTreeMap<u16, Vec<String>>,
}

impl MozcPosCatalog {
    fn labels_for(&self, id: u16) -> Option<&[String]> {
        self.labels_by_id.get(&id).map(Vec::as_slice)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InitialBoundaryDecision {
    AllowInitial,
    RetainNonInitial,
}

/// Exhaustive review decisions for readings whose Mozc rows would otherwise
/// all be hidden at the beginning of a Sakura conversion query.
#[derive(Debug)]
pub struct NonInitialBoundaryPolicy {
    decisions: BTreeMap<(String, String), InitialBoundaryDecision>,
}

/// Observable result of the raw allomorph classifier plus the exhaustive
/// initial-boundary review policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NonInitialBoundaryReport {
    pub raw_non_initial_entries: usize,
    pub raw_non_initial_pairs: usize,
    pub already_initial_pairs: usize,
    pub audited_blocked_pairs: usize,
    pub allowed_initial_pairs: usize,
    pub retained_non_initial_pairs: usize,
    pub released_non_initial_entries: usize,
    pub generated_initial_alias_entries: usize,
    pub remaining_non_initial_entries: usize,
}

/// Parses the canonical, sorted review table for initial-boundary candidates.
///
/// The table deliberately covers every `(reading, surface)` pair that would
/// otherwise disappear completely. This is a fail-closed audit contract, not
/// a small exception overlay: source drift must update the whole review before
/// a new dictionary can be built.
pub fn parse_non_initial_boundary_policy(
    source: &str,
    text: &str,
    expected_mozc_revision: &str,
) -> Result<NonInitialBoundaryPolicy, String> {
    const HEADER: &str = "reading\tsurface\tdecision\tevidence";
    let mut schema_version = None;
    let mut mozc_revision = None;
    let mut saw_header = false;
    let mut previous_key: Option<(String, String)> = None;
    let mut decisions = BTreeMap::new();

    for (zero_based, raw) in text.lines().enumerate() {
        let line_number = zero_based + 1;
        let line = raw.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            let comment = comment.trim();
            if let Some(value) = comment.strip_prefix("schema_version:") {
                set_policy_metadata(
                    source,
                    line_number,
                    "schema_version",
                    &mut schema_version,
                    value.trim(),
                )?;
            } else if let Some(value) = comment.strip_prefix("mozc_revision:") {
                set_policy_metadata(
                    source,
                    line_number,
                    "mozc_revision",
                    &mut mozc_revision,
                    value.trim(),
                )?;
            }
            continue;
        }
        if !saw_header {
            if line != HEADER {
                return Err(format!(
                    "{source}:{line_number}: expected header '{HEADER}'"
                ));
            }
            saw_header = true;
            continue;
        }

        let columns = line.split('\t').collect::<Vec<_>>();
        if columns.len() != 4 {
            return Err(format!(
                "{source}:{line_number}: expected 4 tab-separated columns, found {}",
                columns.len()
            ));
        }
        let reading = columns[0];
        let surface = columns[1];
        let evidence = columns[3];
        if reading.is_empty() || surface.is_empty() || evidence.is_empty() {
            return Err(format!(
                "{source}:{line_number}: reading, surface, and evidence must be non-empty"
            ));
        }
        let decision = match columns[2] {
            "allow-initial" => InitialBoundaryDecision::AllowInitial,
            "retain-non-initial" => InitialBoundaryDecision::RetainNonInitial,
            value => {
                return Err(format!(
                    "{source}:{line_number}: unknown decision '{value}'"
                ));
            }
        };
        let key = (reading.to_owned(), surface.to_owned());
        if previous_key
            .as_ref()
            .is_some_and(|previous| previous >= &key)
        {
            return Err(format!(
                "{source}:{line_number}: policy rows must be strictly sorted by reading and surface"
            ));
        }
        previous_key = Some(key.clone());
        if decisions.insert(key, decision).is_some() {
            return Err(format!(
                "{source}:{line_number}: duplicate reading/surface decision"
            ));
        }
    }

    if !saw_header {
        return Err(format!("{source}: missing policy header"));
    }
    if schema_version.as_deref() != Some("1") {
        return Err(format!(
            "{source}: schema_version must be exactly 1, found {}",
            schema_version.as_deref().unwrap_or("<missing>")
        ));
    }
    let actual_revision = mozc_revision.as_deref().unwrap_or("<missing>");
    if actual_revision != expected_mozc_revision {
        return Err(format!(
            "{source}: mozc_revision '{actual_revision}' does not match pinned revision '{expected_mozc_revision}'"
        ));
    }
    if decisions.is_empty() {
        return Err(format!("{source}: policy has no decisions"));
    }
    Ok(NonInitialBoundaryPolicy { decisions })
}

fn set_policy_metadata(
    source: &str,
    line_number: usize,
    name: &str,
    slot: &mut Option<String>,
    value: &str,
) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{source}:{line_number}: {name} must not be empty"));
    }
    if slot.replace(value.to_owned()).is_some() {
        return Err(format!("{source}:{line_number}: duplicate {name} metadata"));
    }
    Ok(())
}

/// Parses Mozc's `id.def`, keeping the POS fields associated with every
/// connection id.  Reading the pinned taxonomy at build time is more robust
/// than baking a fragile range of numeric ids into Sakura.
pub fn parse_mozc_pos_catalog(source: &str, text: &str) -> Result<MozcPosCatalog, String> {
    let mut catalog = MozcPosCatalog::default();
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
        let labels = definition
            .split(',')
            .map(str::trim)
            .filter(|label| !label.is_empty() && *label != "*")
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if labels.is_empty() {
            return Err(format!(
                "{source}:{line_number}: POS definition has no labels"
            ));
        }
        if catalog.labels_by_id.insert(id, labels).is_some() {
            return Err(format!("{source}:{line_number}: duplicate POS id {id}"));
        }
    }
    if catalog.labels_by_id.is_empty() {
        return Err(format!("{source}: no POS definitions found"));
    }
    Ok(catalog)
}

/// Marks lexical fragments that are meaningful only after preceding text.
///
/// Mozc deliberately carries initial-voicing allomorphs in the same shards as
/// standalone words. Its full converter has enough context to keep those
/// fragments out of an independent BOS conversion, while Sakura materializes
/// every bounded N-best path. Preserve the entries for compound paths, but
/// make their left-boundary contract explicit in the compiled flag.
///
/// The allomorph rule is intentionally POS-aware: a voiced suffix is marked
/// only when the same surface has an unvoiced reading with a retained
/// independent base entry and the voiced reading itself has no independent
/// entry. Suffix, prefix, and non-independent siblings are not enough evidence
/// because users may enter productive bound forms in a separate composition
/// (for example, 「運用」 followed by 「び」 for 「運用日」), and Mozc's
/// connection classes alone do not express that input boundary.
/// This avoids a word list and does not reinterpret ordinary standalone nouns
/// that merely begin with a voiced kana.
pub fn mark_non_initial_allomorphs(
    entries: &mut [SourceEntry],
    pos_catalog: &MozcPosCatalog,
) -> usize {
    mark_non_initial_allomorphs_by(entries, pos_catalog, |_| true)
}

/// Applies the allomorph policy using only rows that survived the former
/// row-based cap as lexical evidence. Candidate rows still come from the full
/// surface-preserving trim, but newly rescued POS siblings cannot turn an
/// existing independently usable reading into a compound-only fragment.
pub fn mark_non_initial_allomorphs_with_legacy_evidence(
    entries: &mut [SourceEntry],
    legacy_evidence: &[bool],
    pos_catalog: &MozcPosCatalog,
) -> Result<usize, String> {
    if entries.len() != legacy_evidence.len() {
        return Err(format!(
            "legacy evidence length {} does not match entry length {}",
            legacy_evidence.len(),
            entries.len()
        ));
    }
    Ok(mark_non_initial_allomorphs_by(
        entries,
        pos_catalog,
        |index| legacy_evidence[index],
    ))
}

/// Runs the raw POS-aware classifier, then applies the exhaustive boundary
/// review for every pair that would otherwise have no initial candidate.
pub fn mark_non_initial_allomorphs_with_boundary_policy(
    entries: &mut Vec<SourceEntry>,
    legacy_evidence: &[bool],
    pos_catalog: &MozcPosCatalog,
    policy: &NonInitialBoundaryPolicy,
) -> Result<NonInitialBoundaryReport, String> {
    mark_non_initial_allomorphs_with_legacy_evidence(entries, legacy_evidence, pos_catalog)?;

    let raw_non_initial_entries = entries
        .iter()
        .filter(|entry| entry.flags.contains(EntryFlags::NON_INITIAL))
        .count();
    let raw_pairs = entries
        .iter()
        .filter(|entry| entry.flags.contains(EntryFlags::NON_INITIAL))
        .map(|entry| (entry.reading.clone(), entry.surface.clone()))
        .collect::<BTreeSet<_>>();
    let initially_available = entries
        .iter()
        .filter(|entry| !entry.flags.contains(EntryFlags::NON_INITIAL))
        .map(|entry| (entry.reading.clone(), entry.surface.clone()))
        .collect::<BTreeSet<_>>();
    let blocked_pairs = raw_pairs
        .difference(&initially_available)
        .cloned()
        .collect::<BTreeSet<_>>();
    let policy_pairs = policy.decisions.keys().cloned().collect::<BTreeSet<_>>();
    let missing = blocked_pairs
        .difference(&policy_pairs)
        .cloned()
        .collect::<Vec<_>>();
    let extra = policy_pairs
        .difference(&blocked_pairs)
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() || !extra.is_empty() {
        return Err(format!(
            "non-initial boundary policy does not exactly cover blocked pairs; missing {} [{}], extra {} [{}]",
            missing.len(),
            summarize_reading_surface_pairs(&missing),
            extra.len(),
            summarize_reading_surface_pairs(&extra)
        ));
    }

    let mut allowed_initial_pairs = 0usize;
    let mut retained_non_initial_pairs = 0usize;
    let mut released_non_initial_entries = 0usize;
    let mut generated_initial_alias_entries = 0usize;
    let mut initial_aliases = Vec::new();
    for (pair, decision) in &policy.decisions {
        match decision {
            InitialBoundaryDecision::AllowInitial => {
                let best_index = entries
                    .iter()
                    .enumerate()
                    .filter(|(_, entry)| {
                        entry.reading == pair.0
                            && entry.surface == pair.1
                            && entry.flags.contains(EntryFlags::NON_INITIAL)
                    })
                    .min_by_key(|(_, entry)| {
                        let labels = pos_catalog.labels_for(entry.left_id);
                        let preference = match labels {
                            Some(labels)
                                if labels.first().is_some_and(|label| label == "名詞")
                                    && has_label(labels, "接尾") =>
                            {
                                0u8
                            }
                            Some(labels) if has_label(labels, "接尾") => 1u8,
                            _ => 2u8,
                        };
                        (preference, entry.word_cost, entry.left_id, entry.right_id)
                    })
                    .map(|(index, _)| index)
                    .ok_or_else(|| {
                        format!(
                            "approved initial pair {}/{} has no non-initial entry",
                            pair.0, pair.1
                        )
                    })?;
                let target_identity = preferred_initial_identity(entries, pair, pos_catalog)
                    .unwrap_or((entries[best_index].left_id, entries[best_index].right_id));
                if let Some(existing_index) = entries.iter().position(|entry| {
                    entry.reading == pair.0
                        && entry.surface == pair.1
                        && entry.left_id == target_identity.0
                        && entry.right_id == target_identity.1
                }) {
                    let entry = &mut entries[existing_index];
                    entry.flags =
                        EntryFlags::from_bits(entry.flags.bits() & !EntryFlags::NON_INITIAL.bits());
                    released_non_initial_entries = released_non_initial_entries.saturating_add(1);
                } else {
                    let mut alias = entries[best_index].clone();
                    alias.left_id = target_identity.0;
                    alias.right_id = target_identity.1;
                    alias.flags =
                        EntryFlags::from_bits(alias.flags.bits() & !EntryFlags::NON_INITIAL.bits());
                    initial_aliases.push(alias);
                    generated_initial_alias_entries =
                        generated_initial_alias_entries.saturating_add(1);
                }
                allowed_initial_pairs = allowed_initial_pairs.saturating_add(1);
            }
            InitialBoundaryDecision::RetainNonInitial => {
                retained_non_initial_pairs = retained_non_initial_pairs.saturating_add(1);
            }
        }
    }
    entries.extend(initial_aliases);
    entries.sort_by(|a, b| {
        (
            &a.reading,
            a.word_cost,
            &a.surface,
            a.left_id,
            a.right_id,
            a.flags.bits(),
        )
            .cmp(&(
                &b.reading,
                b.word_cost,
                &b.surface,
                b.left_id,
                b.right_id,
                b.flags.bits(),
            ))
    });

    for (pair, decision) in &policy.decisions {
        if *decision == InitialBoundaryDecision::AllowInitial
            && !entries.iter().any(|entry| {
                entry.reading == pair.0
                    && entry.surface == pair.1
                    && !entry.flags.contains(EntryFlags::NON_INITIAL)
            })
        {
            return Err(format!(
                "approved initial pair {}/{} remained unavailable",
                pair.0, pair.1
            ));
        }
    }

    let remaining_non_initial_entries = entries
        .iter()
        .filter(|entry| entry.flags.contains(EntryFlags::NON_INITIAL))
        .count();
    Ok(NonInitialBoundaryReport {
        raw_non_initial_entries,
        raw_non_initial_pairs: raw_pairs.len(),
        already_initial_pairs: raw_pairs.intersection(&initially_available).count(),
        audited_blocked_pairs: blocked_pairs.len(),
        allowed_initial_pairs,
        retained_non_initial_pairs,
        released_non_initial_entries,
        generated_initial_alias_entries,
        remaining_non_initial_entries,
    })
}

fn preferred_initial_identity(
    entries: &[SourceEntry],
    pair: &(String, String),
    pos_catalog: &MozcPosCatalog,
) -> Option<(u16, u16)> {
    let unvoiced_readings = unvoiced_initial_readings(&pair.0).collect::<BTreeSet<_>>();
    entries
        .iter()
        .filter(|entry| {
            entry.surface == pair.1
                && unvoiced_readings.contains(&entry.reading)
                && pos_catalog
                    .labels_for(entry.left_id)
                    .is_some_and(is_independent_base_pos)
        })
        .min_by_key(|entry| {
            let labels = pos_catalog
                .labels_for(entry.left_id)
                .expect("filtered entry has POS labels");
            let preference = if labels.first().is_some_and(|label| label == "名詞") {
                0u8
            } else {
                1u8
            };
            (preference, entry.word_cost, entry.left_id, entry.right_id)
        })
        .map(|entry| (entry.left_id, entry.right_id))
}

fn summarize_reading_surface_pairs(pairs: &[(String, String)]) -> String {
    const LIMIT: usize = 8;
    let mut summary = pairs
        .iter()
        .take(LIMIT)
        .map(|(reading, surface)| format!("{reading}/{surface}"))
        .collect::<Vec<_>>()
        .join(", ");
    if pairs.len() > LIMIT {
        summary.push_str(", ...");
    }
    summary
}

fn mark_non_initial_allomorphs_by(
    entries: &mut [SourceEntry],
    pos_catalog: &MozcPosCatalog,
    is_legacy_evidence: impl Fn(usize) -> bool,
) -> usize {
    let identities = entries
        .iter()
        .map(|entry| {
            (
                entry.surface.clone(),
                entry.left_id,
                entry.right_id,
                entry.reading.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    let standalone_surface_readings = entries
        .iter()
        .enumerate()
        .filter(|(index, entry)| {
            is_legacy_evidence(*index)
                && pos_catalog.labels_for(entry.left_id).is_some_and(|labels| {
                    is_independent_base_pos(labels)
                        && !is_voiced_continuative_allomorph(entry, labels, &identities)
                })
        })
        .map(|(_, entry)| (entry.surface.clone(), entry.reading.clone()))
        .collect::<BTreeSet<_>>();
    let mut marked = 0usize;
    for entry in entries {
        let Some(labels) = pos_catalog.labels_for(entry.left_id) else {
            continue;
        };
        let voiced_suffix = has_label(labels, "接尾")
            && !standalone_surface_readings
                .contains(&(entry.surface.clone(), entry.reading.clone()))
            && unvoiced_initial_readings(&entry.reading).any(|reading| {
                standalone_surface_readings.contains(&(entry.surface.clone(), reading))
            });
        let voiced_continuative = is_voiced_continuative_allomorph(entry, labels, &identities);
        if (voiced_suffix || voiced_continuative) && !entry.flags.contains(EntryFlags::NON_INITIAL)
        {
            entry.flags = entry.flags | EntryFlags::NON_INITIAL;
            marked = marked.saturating_add(1);
        }
    }
    marked
}

fn is_voiced_continuative_allomorph(
    entry: &SourceEntry,
    labels: &[String],
    identities: &BTreeSet<(String, u16, u16, String)>,
) -> bool {
    has_label(labels, "連用形")
        && unvoiced_initial_readings(&entry.reading).any(|reading| {
            identities.contains(&(
                entry.surface.clone(),
                entry.left_id,
                entry.right_id,
                reading,
            ))
        })
}

fn is_independent_base_pos(labels: &[String]) -> bool {
    !has_label(labels, "接尾") && !has_label(labels, "接頭詞") && !has_label(labels, "非自立")
}

fn unvoiced_initial_readings(reading: &str) -> impl Iterator<Item = String> + '_ {
    let mut characters = reading.chars();
    let first = characters.next();
    let suffix = characters.as_str();
    unvoiced_initials(first)
        .into_iter()
        .flatten()
        .map(move |initial| {
            let mut value = String::with_capacity(reading.len());
            value.push(initial);
            value.push_str(suffix);
            value
        })
}

fn unvoiced_initials(character: Option<char>) -> [Option<char>; 2] {
    let Some(character) = character else {
        return [None, None];
    };
    let primary = match character {
        'が' => 'か',
        'ぎ' => 'き',
        'ぐ' => 'く',
        'げ' => 'け',
        'ご' => 'こ',
        'ざ' => 'さ',
        'ぜ' => 'せ',
        'ぞ' => 'そ',
        'だ' => 'た',
        'で' => 'て',
        'ど' => 'と',
        'ば' | 'ぱ' => 'は',
        'び' | 'ぴ' => 'ひ',
        'ぶ' | 'ぷ' => 'ふ',
        'べ' | 'ぺ' => 'へ',
        'ぼ' | 'ぽ' => 'ほ',
        'ガ' => 'カ',
        'ギ' => 'キ',
        'グ' => 'ク',
        'ゲ' => 'ケ',
        'ゴ' => 'コ',
        'ザ' => 'サ',
        'ゼ' => 'セ',
        'ゾ' => 'ソ',
        'ダ' => 'タ',
        'デ' => 'テ',
        'ド' => 'ト',
        'バ' | 'パ' => 'ハ',
        'ビ' | 'ピ' => 'ヒ',
        'ブ' | 'プ' => 'フ',
        'ベ' | 'ペ' => 'ヘ',
        'ボ' | 'ポ' => 'ホ',
        'じ' => 'し',
        'ず' => 'す',
        'ぢ' => 'ち',
        'づ' => 'つ',
        'ジ' => 'シ',
        'ズ' => 'ス',
        'ヂ' => 'チ',
        'ヅ' => 'ツ',
        _ => return [None, None],
    };
    let secondary = match character {
        'じ' | 'ぢ' => Some('ち'),
        'ず' | 'づ' => Some('つ'),
        'ジ' | 'ヂ' => Some('チ'),
        'ズ' | 'ヅ' => Some('ツ'),
        _ => None,
    };
    [Some(primary), secondary]
}

/// Categorizes an entry from Sakura's existing system or overlay sources.
///
/// Imported entries arrive with their category already established by their
/// enumerator, so this function only needs the exact Mozc POS taxonomy and
/// conservative surface rules for Sakura's original sources.
pub fn classify_existing_entry(
    entry: &SourceEntry,
    pos_catalog: &MozcPosCatalog,
) -> DictionaryCategory {
    if entry.flags.contains(EntryFlags::IT) {
        return DictionaryCategory::ItEngineering;
    }
    if entry.flags.contains(EntryFlags::SPELLING_CORRECTION) {
        return DictionaryCategory::OrthographyVariants;
    }

    if let Some(labels) = pos_catalog.labels_for(entry.left_id) {
        if has_label(labels, "人名") {
            return DictionaryCategory::PersonNames;
        }
        if has_label(labels, "地名") {
            return DictionaryCategory::PlaceNames;
        }
        if has_label(labels, "組織") {
            return DictionaryCategory::OrganizationsProducts;
        }
        if has_any_label(labels, &["数", "助数詞"]) {
            return DictionaryCategory::NumericTimeUnits;
        }
        if has_any_label(labels, &["記号", "顔文字"]) {
            return DictionaryCategory::SymbolsEmoji;
        }
        if has_any_label(labels, &["動詞", "形容詞", "形容動詞"]) {
            return DictionaryCategory::Inflectional;
        }
        if has_any_label(
            labels,
            &[
                "助詞",
                "助動詞",
                "連体詞",
                "副詞",
                "接続詞",
                "感動詞",
                "接頭詞",
                "接尾",
                "フィラー",
            ],
        ) {
            return DictionaryCategory::GrammarFunction;
        }
    }

    if is_symbol_or_emoji(&entry.surface) {
        return DictionaryCategory::SymbolsEmoji;
    }
    if is_ascii_term(&entry.surface) {
        return DictionaryCategory::AbbreviationsAscii;
    }
    if is_katakana_term(&entry.surface) {
        return DictionaryCategory::KatakanaLoanwords;
    }
    if entry.surface.contains([' ', '\u{3000}']) {
        return DictionaryCategory::FixedExpressions;
    }

    DictionaryCategory::GeneralLexicon
}

/// Japanese prefecture names used to detect full postal-style addresses.
///
/// Matching on a bare `県` would also drop city names such as `山県市`.
const PREFECTURE_NAMES: &[&str] = &[
    "北海道",
    "青森県",
    "岩手県",
    "宮城県",
    "秋田県",
    "山形県",
    "福島県",
    "茨城県",
    "栃木県",
    "群馬県",
    "埼玉県",
    "千葉県",
    "東京都",
    "神奈川県",
    "新潟県",
    "富山県",
    "石川県",
    "福井県",
    "山梨県",
    "長野県",
    "岐阜県",
    "静岡県",
    "愛知県",
    "三重県",
    "滋賀県",
    "京都府",
    "大阪府",
    "兵庫県",
    "奈良県",
    "和歌山県",
    "鳥取県",
    "島根県",
    "岡山県",
    "広島県",
    "山口県",
    "徳島県",
    "香川県",
    "愛媛県",
    "高知県",
    "福岡県",
    "佐賀県",
    "長崎県",
    "熊本県",
    "大分県",
    "宮崎県",
    "鹿児島県",
    "沖縄県",
];

/// Postal-code readings, placeholder readings such as `(そのた)`, and
/// prefecture-qualified municipal addresses (`北海道…市…`, `兵庫県姫路市…`).
///
/// Short toponyms (`東京`, `渋谷`, `横浜`, `渋谷区`, `横浜市`) stay. Digit
/// readings are dropped only inside the place-name category so IT overlays
/// such as `404` are not removed.
pub fn is_address_layer_entry(reading: &str, surface: &str, category: DictionaryCategory) -> bool {
    is_prefecture_address_surface(surface)
        || (category == DictionaryCategory::PlaceNames && is_postal_or_placeholder_reading(reading))
}

fn is_postal_or_placeholder_reading(reading: &str) -> bool {
    if reading.is_empty() {
        return false;
    }
    if reading.starts_with('(') && reading.ends_with(')') {
        return true;
    }
    let mut saw_digit = false;
    for byte in reading.bytes() {
        match byte {
            b'0'..=b'9' => saw_digit = true,
            b'-' => {}
            _ => return false,
        }
    }
    saw_digit
}

fn is_prefecture_address_surface(surface: &str) -> bool {
    has_prefecture_name(surface) && has_municipality_unit(surface)
}

fn has_prefecture_name(surface: &str) -> bool {
    PREFECTURE_NAMES
        .iter()
        .any(|prefecture| surface.contains(prefecture))
}

fn has_municipality_unit(surface: &str) -> bool {
    surface.contains('市')
        || surface.contains('区')
        || surface.contains('郡')
        || surface.contains('町')
        || surface.contains('村')
}

fn has_label(labels: &[String], wanted: &str) -> bool {
    labels.iter().any(|label| label == wanted)
}

fn has_any_label(labels: &[String], wanted: &[&str]) -> bool {
    wanted.iter().any(|wanted| has_label(labels, wanted))
}

fn is_ascii_term(surface: &str) -> bool {
    !surface.is_empty()
        && surface.is_ascii()
        && surface.bytes().any(|byte| byte.is_ascii_alphanumeric())
        && surface
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
}

fn is_katakana_term(surface: &str) -> bool {
    let mut saw_katakana = false;
    for character in surface.chars() {
        if ('\u{30a0}'..='\u{30ff}').contains(&character) {
            saw_katakana = true;
            continue;
        }
        if matches!(
            character,
            ' ' | '\u{3000}' | '！' | '？' | '・' | '＝' | '＋' | '＆'
        ) {
            continue;
        }
        return false;
    }
    saw_katakana
}

fn is_symbol_or_emoji(surface: &str) -> bool {
    surface.chars().any(is_emoji_like)
        || (!surface.is_empty() && surface.chars().all(is_symbol_like))
}

fn is_emoji_like(character: char) -> bool {
    matches!(
        character as u32,
        0x1f000..=0x1faff | 0x2600..=0x27bf | 0x2b00..=0x2bff
    )
}

fn is_symbol_like(character: char) -> bool {
    character.is_ascii_punctuation()
        || matches!(
            character,
            '\u{3000}'..='\u{303f}'
                | '\u{2190}'..='\u{21ff}'
                | '\u{2200}'..='\u{22ff}'
                | '\u{2500}'..='\u{257f}'
                | '\u{25a0}'..='\u{25ff}'
                | '\u{ff01}'..='\u{ff0f}'
                | '\u{ff1a}'..='\u{ff20}'
                | '\u{ff3b}'..='\u{ff40}'
                | '\u{ff5b}'..='\u{ff65}'
        )
}

#[cfg(test)]
mod tests {
    use sakura_core::dictionary::EntryFlags;

    use super::{
        classify_existing_entry, is_address_layer_entry, mark_non_initial_allomorphs,
        mark_non_initial_allomorphs_with_boundary_policy,
        mark_non_initial_allomorphs_with_legacy_evidence, parse_mozc_pos_catalog,
        parse_non_initial_boundary_policy, DictionaryCategory,
    };
    use crate::{entries_to_category_tsv, parse_category_entries, parse_entries};

    fn entry(left_id: u16, surface: &str, flags: &str) -> crate::SourceEntry {
        let tsv = format!(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nよみ\t{surface}\t{left_id}\t{left_id}\t100\t-\t{flags}\t\n"
        );
        parse_entries("fixture.tsv", &tsv)
            .expect("fixture parses")
            .remove(0)
    }

    fn catalog() -> super::MozcPosCatalog {
        parse_mozc_pos_catalog(
            "id.def",
            "10 名詞,固有名詞,人名,姓\n11 名詞,固有名詞,地名,一般\n12 名詞,固有名詞,組織,*\n13 名詞,数,*,*\n14 記号,一般,*,*\n15 動詞,自立,*,*\n16 助詞,係助詞,*,*\n",
        )
        .expect("fixture POS catalog parses")
    }

    fn lexical_entry(reading: &str, surface: &str, id: u16) -> crate::SourceEntry {
        let tsv = format!(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n{reading}\t{surface}\t{id}\t{id}\t100\t1300\tpredict\t\n"
        );
        parse_entries("fixture.tsv", &tsv)
            .expect("fixture parses")
            .remove(0)
    }

    #[test]
    fn non_initial_marking_uses_pos_and_identity_instead_of_surface_blacklists() {
        let catalog = parse_mozc_pos_catalog(
            "id.def",
            "829 動詞,自立,*,*,五段・ワ行促音便,連用形,*\n1949 名詞,接尾,一般,*,*,*,*\n1851 名詞,一般,*,*,*,*,*\n",
        )
        .expect("fixture POS catalog parses");
        let mut entries = vec![
            lexical_entry("つかい", "使い", 829),
            lexical_entry("ずかい", "使い", 829),
            lexical_entry("つかい", "遣い", 829),
            lexical_entry("づかい", "遣い", 829),
            lexical_entry("ずかい", "遣い", 1949),
            lexical_entry("いし", "石", 1949),
            lexical_entry("から", "柄", 1851),
            lexical_entry("がら", "柄", 1851),
            lexical_entry("きづかい", "気遣い", 1851),
        ];

        assert_eq!(mark_non_initial_allomorphs(&mut entries, &catalog), 3);
        let marked = entries
            .iter()
            .filter(|entry| entry.flags.contains(EntryFlags::NON_INITIAL))
            .map(|entry| {
                (
                    entry.reading.as_str(),
                    entry.surface.as_str(),
                    entry.left_id,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            marked,
            vec![
                ("ずかい", "使い", 829),
                ("づかい", "遣い", 829),
                ("ずかい", "遣い", 1949),
            ]
        );
        assert!(entries
            .iter()
            .all(|entry| entry.flags.contains(EntryFlags::PREDICTION)));
    }

    #[test]
    fn suffix_marking_requires_an_independent_unvoiced_base_entry() {
        let catalog = parse_mozc_pos_catalog(
            "id.def",
            "1949 名詞,接尾,一般,*,*,*,*\n829 動詞,自立,*,*,*,連用形,*\n1197 動詞,非自立,*,*,一段,未然形,替える\n1851 名詞,一般,*,*,*,*,*\n2011 名詞,接尾,助数詞,*,*,*,*\n2115 名詞,非自立,一般,*,*,*,方\n2116 名詞,非自立,一般,*,*,*,日\n3000 接頭詞,名詞接続,*,*,*,*,*\n",
        )
        .expect("fixture POS catalog parses");
        let mut entries = vec![
            // An independent entry for the voiced reading proves that the
            // surface is usable at BOS even though suffix identities coexist.
            lexical_entry("ばん", "版", 1949),
            lexical_entry("ばん", "版", 1851),
            lexical_entry("はん", "版", 1949),
            lexical_entry("はん", "版", 1851),
            // The unvoiced readings have independent base identities, so the
            // voiced suffix allomorphs remain compound-only.
            lexical_entry("ずかい", "使い", 1949),
            lexical_entry("つかい", "使い", 829),
            lexical_entry("つかい", "使い", 1851),
            lexical_entry("ずかい", "遣い", 1949),
            lexical_entry("つかい", "遣い", 829),
            // A suffix-only pair is another standalone-homograph guard.
            lexical_entry("ぐち", "口", 1949),
            lexical_entry("くち", "口", 1949),
            // Non-independent and prefix readings do not prove that the
            // voiced form is merely an invalid word-initial fragment.
            lexical_entry("び", "日", 1949),
            lexical_entry("ぴ", "日", 2011),
            lexical_entry("ひ", "日", 2116),
            lexical_entry("どき", "時", 1949),
            lexical_entry("とき", "時", 2115),
            lexical_entry("がえ", "替え", 1949),
            lexical_entry("かえ", "替え", 1197),
            lexical_entry("ぜん", "前", 1949),
            lexical_entry("せん", "前", 3000),
        ];

        assert_eq!(mark_non_initial_allomorphs(&mut entries, &catalog), 2);
        assert!(entries.iter().any(|entry| {
            entry.reading == "ばん"
                && entry.surface == "版"
                && entry.left_id == 1949
                && !entry.flags.contains(EntryFlags::NON_INITIAL)
        }));
        assert!(entries.iter().any(|entry| {
            entry.reading == "ずかい"
                && entry.surface == "使い"
                && entry.flags.contains(EntryFlags::NON_INITIAL)
        }));
        assert!(entries.iter().any(|entry| {
            entry.reading == "ずかい"
                && entry.surface == "遣い"
                && entry.flags.contains(EntryFlags::NON_INITIAL)
        }));
        assert!(entries.iter().any(|entry| {
            entry.reading == "ぐち"
                && entry.surface == "口"
                && !entry.flags.contains(EntryFlags::NON_INITIAL)
        }));
        for (reading, surface) in [
            ("び", "日"),
            ("ぴ", "日"),
            ("どき", "時"),
            ("がえ", "替え"),
            ("ぜん", "前"),
        ] {
            assert!(entries.iter().any(|entry| {
                entry.reading == reading
                    && entry.surface == surface
                    && !entry.flags.contains(EntryFlags::NON_INITIAL)
            }));
        }
    }

    #[test]
    fn surface_rescued_pos_rows_do_not_reclassify_existing_bound_forms() {
        let catalog = parse_mozc_pos_catalog(
            "id.def",
            "1851 名詞,一般,*,*,*,*,*\n1949 名詞,接尾,一般,*,*,*,*\n2116 名詞,非自立,一般,*,*,*,日\n",
        )
        .expect("fixture POS catalog parses");
        let mut entries = vec![
            lexical_entry("び", "日", 1949),
            lexical_entry("ひ", "日", 2116),
            lexical_entry("ひ", "日", 1851),
        ];

        let marked = mark_non_initial_allomorphs_with_legacy_evidence(
            &mut entries,
            &[true, true, false],
            &catalog,
        )
        .expect("aligned evidence");

        assert_eq!(marked, 0);
        assert!(entries
            .iter()
            .all(|entry| !entry.flags.contains(EntryFlags::NON_INITIAL)));
        assert!(
            mark_non_initial_allomorphs_with_legacy_evidence(&mut entries, &[true], &catalog,)
                .is_err()
        );
    }

    #[test]
    fn exhaustive_boundary_policy_releases_only_one_reviewed_identity_per_pair() {
        let catalog = parse_mozc_pos_catalog(
            "id.def",
            "829 動詞,自立,*,*,五段・ワ行促音便,連用形,*\n1949 名詞,接尾,一般,*,*,*,*\n1851 名詞,一般,*,*,*,*,*\n",
        )
        .expect("fixture POS catalog parses");
        let mut entries = vec![
            lexical_entry("つかい", "使い", 829),
            lexical_entry("つかい", "使い", 1851),
            lexical_entry("ずかい", "使い", 829),
            lexical_entry("ずかい", "使い", 1949),
            lexical_entry("すみ", "済み", 829),
            lexical_entry("ずみ", "済み", 1949),
            lexical_entry("ふり", "振り", 829),
            lexical_entry("ふり", "振り", 1851),
            lexical_entry("ぶり", "振り", 829),
            lexical_entry("ぶり", "振り", 1949),
        ];
        let policy = parse_non_initial_boundary_policy(
            "policy.tsv",
            "# schema_version: 1\n# mozc_revision: test-revision\nreading\tsurface\tdecision\tevidence\nずかい\t使い\tretain-non-initial\tfragment-guard\nずみ\t済み\tallow-initial\tcommon-form\nぶり\t振り\tallow-initial\tcommon-form\n",
            "test-revision",
        )
        .expect("policy parses");
        let report = mark_non_initial_allomorphs_with_boundary_policy(
            &mut entries,
            &[true; 10],
            &catalog,
            &policy,
        )
        .expect("complete policy applies");

        assert_eq!(report.raw_non_initial_entries, 5);
        assert_eq!(report.raw_non_initial_pairs, 3);
        assert_eq!(report.already_initial_pairs, 0);
        assert_eq!(report.audited_blocked_pairs, 3);
        assert_eq!(report.allowed_initial_pairs, 2);
        assert_eq!(report.retained_non_initial_pairs, 1);
        assert_eq!(report.released_non_initial_entries, 0);
        assert_eq!(report.generated_initial_alias_entries, 2);
        assert_eq!(report.remaining_non_initial_entries, 5);
        let released_buri = entries
            .iter()
            .filter(|entry| {
                entry.reading == "ぶり"
                    && entry.surface == "振り"
                    && !entry.flags.contains(EntryFlags::NON_INITIAL)
            })
            .collect::<Vec<_>>();
        assert_eq!(released_buri.len(), 1);
        assert_eq!(released_buri[0].left_id, 1851);
        assert!(entries.iter().any(|entry| {
            entry.reading == "ぶり"
                && entry.surface == "振り"
                && entry.left_id == 829
                && entry.flags.contains(EntryFlags::NON_INITIAL)
        }));
        assert!(entries.iter().any(|entry| {
            entry.reading == "ずみ"
                && entry.surface == "済み"
                && !entry.flags.contains(EntryFlags::NON_INITIAL)
        }));
        assert!(entries
            .iter()
            .filter(|entry| { entry.reading == "ずかい" && entry.surface == "使い" })
            .all(|entry| entry.flags.contains(EntryFlags::NON_INITIAL)));
    }

    #[test]
    fn boundary_policy_fails_closed_on_source_drift_and_bad_metadata() {
        let catalog = parse_mozc_pos_catalog(
            "id.def",
            "829 動詞,自立,*,*,五段・ワ行促音便,連用形,*\n1949 名詞,接尾,一般,*,*,*,*\n1851 名詞,一般,*,*,*,*,*\n",
        )
        .expect("fixture POS catalog parses");
        let mut entries = vec![
            lexical_entry("つかい", "使い", 829),
            lexical_entry("つかい", "使い", 1851),
            lexical_entry("ずかい", "使い", 829),
            lexical_entry("ずかい", "使い", 1949),
            lexical_entry("すみ", "済み", 829),
            lexical_entry("ずみ", "済み", 1949),
        ];
        let incomplete = parse_non_initial_boundary_policy(
            "policy.tsv",
            "# schema_version: 1\n# mozc_revision: test-revision\nreading\tsurface\tdecision\tevidence\nずみ\t済み\tallow-initial\tcommon-form\n",
            "test-revision",
        )
        .expect("incomplete policy is syntactically valid");
        let error = mark_non_initial_allomorphs_with_boundary_policy(
            &mut entries,
            &[true; 6],
            &catalog,
            &incomplete,
        )
        .expect_err("missing blocked pair must fail");
        assert!(error.contains("missing 1 [ずかい/使い]"), "{error}");

        let wrong_revision = parse_non_initial_boundary_policy(
            "policy.tsv",
            "# schema_version: 1\n# mozc_revision: stale\nreading\tsurface\tdecision\tevidence\nずみ\t済み\tallow-initial\tcommon-form\n",
            "test-revision",
        )
        .expect_err("stale Mozc identity must fail");
        assert!(wrong_revision.contains("does not match pinned revision"));
        let unsorted = parse_non_initial_boundary_policy(
            "policy.tsv",
            "# schema_version: 1\n# mozc_revision: test-revision\nreading\tsurface\tdecision\tevidence\nぶり\t振り\tallow-initial\tcommon-form\nずみ\t済み\tallow-initial\tcommon-form\n",
            "test-revision",
        )
        .expect_err("non-canonical order must fail");
        assert!(unsorted.contains("strictly sorted"));
    }

    #[test]
    fn category_names_are_clear_and_source_neutral() {
        assert_eq!(
            DictionaryCategory::GrammarFunction.file_name(),
            "01_文法・機能語.tsv"
        );
        assert_eq!(
            DictionaryCategory::ItEngineering.file_name(),
            "11_IT・技術用語.tsv"
        );
        for category in DictionaryCategory::ALL {
            assert!(!category.file_name().contains("ATOK"));
            assert!(!category.file_name().contains("ATDELIB"));
            assert!(category.file_name().ends_with(".tsv"));
        }
    }

    #[test]
    fn existing_entries_use_exact_pos_before_surface_heuristics() {
        let catalog = catalog();
        assert_eq!(
            classify_existing_entry(&entry(10, "山田", ""), &catalog),
            DictionaryCategory::PersonNames
        );
        assert_eq!(
            classify_existing_entry(&entry(11, "東京", ""), &catalog),
            DictionaryCategory::PlaceNames
        );
        assert_eq!(
            classify_existing_entry(&entry(12, "桜入力", ""), &catalog),
            DictionaryCategory::OrganizationsProducts
        );
        assert_eq!(
            classify_existing_entry(&entry(13, "三", ""), &catalog),
            DictionaryCategory::NumericTimeUnits
        );
        assert_eq!(
            classify_existing_entry(&entry(14, "☆", ""), &catalog),
            DictionaryCategory::SymbolsEmoji
        );
        assert_eq!(
            classify_existing_entry(&entry(15, "書く", ""), &catalog),
            DictionaryCategory::Inflectional
        );
        assert_eq!(
            classify_existing_entry(&entry(16, "は", ""), &catalog),
            DictionaryCategory::GrammarFunction
        );
    }

    #[test]
    fn existing_entry_surface_and_flag_fallbacks_are_stable() {
        let catalog = catalog();
        assert_eq!(
            classify_existing_entry(&entry(999, "SakuraInput", ""), &catalog),
            DictionaryCategory::AbbreviationsAscii
        );
        assert_eq!(
            classify_existing_entry(&entry(999, "コンパイラ", ""), &catalog),
            DictionaryCategory::KatakanaLoanwords
        );
        assert_eq!(
            classify_existing_entry(&entry(999, "表記 読み", ""), &catalog),
            DictionaryCategory::FixedExpressions
        );
        assert_eq!(
            classify_existing_entry(&entry(999, "専門", "it"), &catalog),
            DictionaryCategory::ItEngineering
        );
        assert_eq!(
            classify_existing_entry(&entry(999, "誤り", "correction"), &catalog),
            DictionaryCategory::OrthographyVariants
        );
    }

    #[test]
    fn category_tsv_is_header_only_and_rejects_metadata() {
        let category_tsv =
            entries_to_category_tsv(&[entry(999, "語", "")]).expect("category TSV serializes");
        assert!(category_tsv.starts_with("reading\tsurface\t"));
        assert!(!category_tsv.contains("# license:"));
        assert_eq!(
            parse_category_entries("category.tsv", &category_tsv)
                .expect("header-only category TSV parses")
                .len(),
            1
        );
        assert!(parse_category_entries(
            "category.tsv",
            "# source: unwanted\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n"
        )
        .is_err());
    }

    fn drops(reading: &str, surface: &str) -> bool {
        is_address_layer_entry(reading, surface, DictionaryCategory::PlaceNames)
    }

    #[test]
    fn address_layer_drops_postal_codes_and_full_prefecture_addresses() {
        assert!(drops("001", "北海道札幌市北区"));
        assert!(drops("001-0000", "北海道札幌市北区北一条西"));
        assert!(drops("010-01", "秋田県潟上市"));
        assert!(drops("(そのた)", "北海道苫小牧市晴海町"));
        assert!(drops("よこはま", "北海道厚岸郡浜中町横浜"));
        assert!(drops("よこはま", "兵庫県姫路市網干区余子浜"));
        assert!(drops("あい", "大阪府茨木市安威"));
        assert!(drops("ちよだく", "東京都千代田区"));
    }

    #[test]
    fn address_layer_keeps_short_place_names() {
        assert!(!drops("とうきょう", "東京"));
        assert!(!drops("とうきょうと", "東京都"));
        assert!(!drops("しぶや", "渋谷"));
        assert!(!drops("しぶやく", "渋谷区"));
        assert!(!drops("よこはま", "横浜"));
        assert!(!drops("よこはまし", "横浜市"));
        assert!(!drops("おおさか", "大阪"));
        assert!(!drops("おおさかし", "大阪市"));
        assert!(!drops("きょうとし", "京都市"));
        assert!(!drops("さっぽろ", "札幌"));
        assert!(!drops("ほっかいどう", "北海道"));
        assert!(!drops("あいあいちょう", "相合町"));
        assert!(!drops("やまがたし", "山県市"));
    }

    #[test]
    fn address_layer_does_not_drop_digit_readings_outside_place_names() {
        assert!(!is_address_layer_entry(
            "404",
            "404",
            DictionaryCategory::ItEngineering
        ));
        assert!(is_address_layer_entry(
            "404",
            "404",
            DictionaryCategory::PlaceNames
        ));
    }
}
