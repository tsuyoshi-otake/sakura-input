//! Exhaustive compound-homophone probe: cheaper same-reading words stealing
//! noun-noun conversions (昨日紹介 for 機能紹介, 全昨日紹介 for 全機能紹介).
//!
//! Offline only. Reads a compiled `system.dic` and never talks to the engine,
//! history, or a user dictionary.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use sakura_core::dictionary::EntryFlags;
use sakura_core::{ConversionOptions, Converter, Dictionary};

const TARGET_CASES: usize = 10_000;
const LEXICON_CAP: usize = 2_500;
const MAX_CANDIDATES: usize = 9;
const MIN_GAP: i64 = 300;
const IT_BIAS_PER_MILLE: i64 = 100;
const MAX_IT_BOOST: i64 = 800;

const PREFIXES: &[(&str, &str)] = &[
    ("ぜん", "全"),
    ("しん", "新"),
    ("ほん", "本"),
    ("かく", "各"),
    ("み", "未"),
    ("さい", "再"),
    ("しゅ", "主"),
    ("ふく", "副"),
    ("こう", "高"),
    ("てい", "低"),
    ("げん", "現"),
    ("きゅう", "旧"),
    ("だい", "大"),
];

const SEED_PAIRS: &[(&str, &str, &str, &str)] = &[
    ("きのう", "機能", "しょうかい", "紹介"),
    ("きのう", "機能", "がいよう", "概要"),
    ("きのう", "機能", "せっけい", "設計"),
    ("きのう", "機能", "いちらん", "一覧"),
];

#[derive(Clone, Debug)]
struct Word {
    reading: String,
    surface: String,
    cost: i32,
    effective: i64,
    it: bool,
}

#[derive(Clone, Debug)]
struct Group {
    predator: Word,
    prey: Vec<Word>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Seed,
    Lexicon,
    Pair,
    Prefix,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Seed => "seed",
            Self::Lexicon => "lexicon",
            Self::Pair => "pair",
            Self::Prefix => "prefix",
        }
    }
}

struct Case {
    kind: Kind,
    reading: String,
    expected: String,
    predator: String,
    prey: String,
    gap: i64,
}

struct Hit {
    kind: Kind,
    reading: String,
    expected: String,
    top1: String,
    intended_rank: i32,
    predator: String,
    prey: String,
    gap: i64,
    steal: bool,
}

struct Options {
    dictionary: PathBuf,
    report: PathBuf,
    limit: usize,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("compound-homophone-scan: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let options = parse_options(std::env::args_os().skip(1))?;
    let started = Instant::now();
    let bytes = std::fs::read(&options.dictionary)
        .map_err(|error| format!("read {}: {error}", options.dictionary.display()))?;
    let dictionary = Dictionary::parse(&bytes)
        .map_err(|error| format!("parse {}: {error}", options.dictionary.display()))?;

    let (words, groups, exact_counts) = collect_words(&dictionary)?;
    let cases = build_cases(&dictionary, &words, &groups, options.limit)?;
    let hits = convert_cases(&dictionary, &cases)?;
    write_report(
        &options.report,
        &groups,
        &exact_counts,
        &hits,
        started.elapsed().as_secs_f64(),
    )?;
    print_summary(&groups, &exact_counts, &hits, cases.len());
    println!("report: {}", options.report.display());
    Ok(())
}

fn parse_options(args: impl Iterator<Item = std::ffi::OsString>) -> Result<Options, String> {
    let mut dictionary = None;
    let mut report = None;
    let mut limit = TARGET_CASES;
    let mut args = args.peekable();
    while let Some(argument) = args.next() {
        let Some(name) = argument.to_str() else {
            return Err("arguments must be valid Unicode".to_owned());
        };
        match name {
            "--dictionary" => {
                dictionary = Some(next_value(&mut args, name)?);
            }
            "--report" => {
                report = Some(next_value(&mut args, name)?);
            }
            "--limit" => {
                let value = next_value(&mut args, name)?;
                limit = value
                    .parse()
                    .map_err(|_| "--limit must be a positive integer".to_owned())?;
                if limit == 0 {
                    return Err("--limit must be a positive integer".into());
                }
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Options {
        dictionary: PathBuf::from(dictionary.ok_or("--dictionary is required")?),
        report: PathBuf::from(report.ok_or("--report is required")?),
        limit,
    })
}

fn next_value(
    args: &mut std::iter::Peekable<impl Iterator<Item = std::ffi::OsString>>,
    name: &str,
) -> Result<String, String> {
    let value = args
        .next()
        .ok_or_else(|| format!("{name} requires a value"))?;
    value
        .into_string()
        .map_err(|_| format!("{name} must be valid Unicode"))
}

type WordCollection = (Vec<Word>, BTreeMap<String, Group>, BTreeMap<String, usize>);

fn collect_words(dictionary: &Dictionary<'_>) -> Result<WordCollection, String> {
    let mut best: BTreeMap<(String, String), Word> = BTreeMap::new();
    let mut exact_counts: BTreeMap<String, usize> = BTreeMap::new();
    dictionary
        .visit_entries(|reading, entry| {
            if entry.flags.contains(EntryFlags::SPELLING_CORRECTION)
                || entry.word_cost < 0
                || !is_kana_reading(reading)
            {
                return true;
            }
            let mut surface = String::new();
            if dictionary.write_surface(entry, &mut surface).is_err() {
                return true;
            }
            if surface.is_empty() || is_ascii_digit(&surface) || !has_kanji(&surface) {
                return true;
            }
            *exact_counts.entry(reading.to_owned()).or_default() += 1;
            let it = entry.flags.contains(EntryFlags::IT);
            let word = Word {
                reading: reading.to_owned(),
                surface: surface.clone(),
                cost: entry.word_cost,
                effective: effective_cost(entry.word_cost, it),
                it,
            };
            best.entry((reading.to_owned(), surface))
                .and_modify(|existing| {
                    if word.effective < existing.effective {
                        existing.cost = word.cost;
                        existing.effective = word.effective;
                    }
                    existing.it |= word.it;
                    existing.effective = effective_cost(existing.cost, existing.it);
                })
                .or_insert(word);
            true
        })
        .map_err(|error| format!("visit entries: {error}"))?;

    let mut by_reading: BTreeMap<String, Vec<Word>> = BTreeMap::new();
    for word in best.into_values() {
        by_reading
            .entry(word.reading.clone())
            .or_default()
            .push(word);
    }

    let mut groups = BTreeMap::new();
    let mut words = Vec::new();
    for (reading, mut surfaces) in by_reading {
        surfaces.sort_by(|left, right| {
            left.effective
                .cmp(&right.effective)
                .then_with(|| left.surface.cmp(&right.surface))
        });
        if let Some(predator) = surfaces.first().cloned() {
            let prey = surfaces
                .iter()
                .skip(1)
                .filter(|word| word.effective - predator.effective >= MIN_GAP)
                .cloned()
                .collect::<Vec<_>>();
            if !prey.is_empty() {
                groups.insert(reading, Group { predator, prey });
            }
        }
        words.extend(surfaces);
    }
    Ok((words, groups, exact_counts))
}

fn build_cases(
    dictionary: &Dictionary<'_>,
    words: &[Word],
    groups: &BTreeMap<String, Group>,
    limit: usize,
) -> Result<Vec<Case>, String> {
    let mut seen = BTreeSet::new();
    let mut cases = Vec::new();

    for (first_reading, first_surface, second_reading, second_surface) in SEED_PAIRS {
        let Some(group) = groups.get(*first_reading) else {
            continue;
        };
        if !group.prey.iter().any(|word| word.surface == *first_surface) {
            continue;
        }
        push_case(
            &mut cases,
            &mut seen,
            Case {
                kind: Kind::Seed,
                reading: format!("{first_reading}{second_reading}"),
                expected: format!("{first_surface}{second_surface}"),
                predator: group.predator.surface.clone(),
                prey: (*first_surface).to_owned(),
                gap: group
                    .prey
                    .iter()
                    .find(|word| word.surface == *first_surface)
                    .map(|word| word.effective - group.predator.effective)
                    .unwrap_or(0),
            },
        );
        if let Some(prefix) = PREFIXES.iter().find(|(reading, _)| *reading == "ぜん") {
            push_case(
                &mut cases,
                &mut seen,
                Case {
                    kind: Kind::Seed,
                    reading: format!("{}{first_reading}{second_reading}", prefix.0),
                    expected: format!("{}{first_surface}{second_surface}", prefix.1),
                    predator: group.predator.surface.clone(),
                    prey: (*first_surface).to_owned(),
                    gap: group
                        .prey
                        .iter()
                        .find(|word| word.surface == *first_surface)
                        .map(|word| word.effective - group.predator.effective)
                        .unwrap_or(0),
                },
            );
        }
    }

    let mut by_reading: BTreeMap<&str, Vec<&Word>> = BTreeMap::new();
    for word in words {
        by_reading
            .entry(word.reading.as_str())
            .or_default()
            .push(word);
    }
    for list in by_reading.values_mut() {
        list.sort_by(|left, right| {
            left.effective
                .cmp(&right.effective)
                .then_with(|| left.surface.cmp(&right.surface))
        });
    }

    let mut lexicon = Vec::new();
    dictionary
        .visit_entries(|reading, entry| {
            if !entry.flags.contains(EntryFlags::IT) || !is_kana_reading(reading) {
                return true;
            }
            let mut surface = String::new();
            if dictionary.write_surface(entry, &mut surface).is_err() || !has_kanji(&surface) {
                return true;
            }
            for prefix in reading_prefixes(reading) {
                let Some(group) = groups.get(prefix) else {
                    continue;
                };
                for prey in &group.prey {
                    if reading == prey.reading || !surface.starts_with(&prey.surface) {
                        continue;
                    }
                    lexicon.push(Case {
                        kind: Kind::Lexicon,
                        reading: reading.to_owned(),
                        expected: surface.clone(),
                        predator: group.predator.surface.clone(),
                        prey: prey.surface.clone(),
                        gap: prey.effective - group.predator.effective,
                    });
                }
            }
            true
        })
        .map_err(|error| format!("visit lexicon compounds: {error}"))?;
    lexicon.sort_by(|left, right| {
        right
            .gap
            .cmp(&left.gap)
            .then_with(|| left.reading.cmp(&right.reading))
            .then_with(|| left.expected.cmp(&right.expected))
    });
    let lexicon_budget = LEXICON_CAP.min(limit.saturating_sub(cases.len()));
    let mut lexicon_kept = 0usize;
    for case in lexicon {
        if lexicon_kept >= lexicon_budget {
            break;
        }
        let before = cases.len();
        push_case(&mut cases, &mut seen, case);
        if cases.len() > before {
            lexicon_kept += 1;
        }
    }

    let prey_ranked = ranked_prey(groups);
    let suffixes = ranked_suffixes(words, groups, &by_reading);
    let prefix_words = PREFIXES
        .iter()
        .filter(|(reading, surface)| {
            by_reading
                .get(reading)
                .is_some_and(|list| list.iter().any(|word| word.surface == *surface))
        })
        .copied()
        .collect::<Vec<_>>();

    let prefix_budget = 1_500.min(limit.saturating_sub(cases.len()));
    let mut prefix_kept = 0usize;
    let prefix_prey = prey_ranked.iter().take(40).cloned().collect::<Vec<_>>();
    let prefix_suffixes = suffixes.iter().take(8).cloned().collect::<Vec<_>>();
    for (prefix_reading, prefix_surface) in &prefix_words {
        if prefix_kept >= prefix_budget {
            break;
        }
        for prey in &prefix_prey {
            let Some(group) = groups.get(&prey.reading) else {
                continue;
            };
            for suffix in &prefix_suffixes {
                if prefix_kept >= prefix_budget {
                    break;
                }
                let before = cases.len();
                push_case(
                    &mut cases,
                    &mut seen,
                    Case {
                        kind: Kind::Prefix,
                        reading: format!("{prefix_reading}{}{}", prey.reading, suffix.reading),
                        expected: format!("{prefix_surface}{}{}", prey.surface, suffix.surface),
                        predator: group.predator.surface.clone(),
                        prey: prey.surface.clone(),
                        gap: prey.effective - group.predator.effective,
                    },
                );
                if cases.len() > before {
                    prefix_kept += 1;
                }
            }
        }
    }

    for prey in &prey_ranked {
        if cases.len() >= limit {
            break;
        }
        let Some(group) = groups.get(&prey.reading) else {
            continue;
        };
        for suffix in &suffixes {
            if suffix.reading == prey.reading {
                continue;
            }
            push_case(
                &mut cases,
                &mut seen,
                Case {
                    kind: Kind::Pair,
                    reading: format!("{}{}", prey.reading, suffix.reading),
                    expected: format!("{}{}", prey.surface, suffix.surface),
                    predator: group.predator.surface.clone(),
                    prey: prey.surface.clone(),
                    gap: prey.effective - group.predator.effective,
                },
            );
            if cases.len() >= limit {
                break;
            }
        }
    }

    if cases.len() > limit {
        cases.truncate(limit);
    }
    Ok(cases)
}

fn ranked_prey(groups: &BTreeMap<String, Group>) -> Vec<Word> {
    let mut prey = Vec::new();
    for group in groups.values() {
        if !is_short_kanji_noun(&group.predator.surface) {
            continue;
        }
        for word in &group.prey {
            if !is_short_kanji_noun(&word.surface) {
                continue;
            }
            let length_delta = (word.surface.chars().count() as i32
                - group.predator.surface.chars().count() as i32)
                .abs();
            if length_delta > 1 {
                continue;
            }
            prey.push(word.clone());
        }
    }
    prey.sort_by(|left, right| {
        let left_group = groups.get(&left.reading).expect("prey reading");
        let right_group = groups.get(&right.reading).expect("prey reading");
        let left_cross = left.it && !left_group.predator.it;
        let right_cross = right.it && !right_group.predator.it;
        right_cross
            .cmp(&left_cross)
            .then_with(|| right.it.cmp(&left.it))
            .then_with(|| {
                (right.effective - right_group.predator.effective)
                    .cmp(&(left.effective - left_group.predator.effective))
            })
            .then_with(|| left.reading.cmp(&right.reading))
            .then_with(|| left.surface.cmp(&right.surface))
    });
    if !prey
        .iter()
        .any(|word| word.reading == "きのう" && word.surface == "機能")
    {
        if let Some(group) = groups.get("きのう") {
            if let Some(word) = group.prey.iter().find(|word| word.surface == "機能") {
                prey.insert(0, word.clone());
            }
        }
    }
    prey.truncate(80);
    prey
}

fn ranked_suffixes(
    words: &[Word],
    groups: &BTreeMap<String, Group>,
    by_reading: &BTreeMap<&str, Vec<&Word>>,
) -> Vec<Word> {
    let mut suffixes = Vec::new();
    for word in words {
        if !is_short_kanji_noun(&word.surface) {
            continue;
        }
        let chars = word.surface.chars().count();
        let reading_chars = word.reading.chars().count();
        if !(2..=3).contains(&chars) || !(3..=6).contains(&reading_chars) {
            continue;
        }
        if groups
            .get(&word.reading)
            .is_some_and(|group| group.predator.surface != word.surface)
        {
            continue;
        }
        let unique_cheapest = by_reading.get(word.reading.as_str()).is_some_and(|list| {
            list.first()
                .is_some_and(|cheapest| cheapest.surface == word.surface)
        });
        if !unique_cheapest {
            continue;
        }
        suffixes.push(word.clone());
    }
    suffixes.sort_by(|left, right| {
        left.effective
            .cmp(&right.effective)
            .then_with(|| left.reading.cmp(&right.reading))
            .then_with(|| left.surface.cmp(&right.surface))
    });
    suffixes.dedup_by(|left, right| left.reading == right.reading && left.surface == right.surface);
    suffixes.truncate(80);
    if !suffixes.iter().any(|word| word.surface == "紹介") {
        if let Some(word) = words
            .iter()
            .find(|word| word.reading == "しょうかい" && word.surface == "紹介")
        {
            suffixes.insert(0, word.clone());
            suffixes.truncate(80);
        }
    }
    suffixes
}

fn is_short_kanji_noun(value: &str) -> bool {
    let chars = value.chars().count();
    (1..=4).contains(&chars)
        && value
            .chars()
            .all(|character| matches!(character, '\u{4E00}'..='\u{9FFF}' | '々'))
}

fn convert_cases(dictionary: &Dictionary<'_>, cases: &[Case]) -> Result<Vec<Hit>, String> {
    let mut converter = Converter::new();
    let options = ConversionOptions {
        max_candidates: MAX_CANDIDATES,
        ..ConversionOptions::default()
    };
    let mut hits = Vec::with_capacity(cases.len());
    for (index, case) in cases.iter().enumerate() {
        if index % 500 == 0 {
            eprintln!("converting {}/{}", index, cases.len());
        }
        let candidates = converter
            .convert(dictionary, &case.reading, options)
            .map_err(|error| format!("{}: {error}", case.reading))?;
        let top1 = candidates
            .first()
            .map(|candidate| candidate.text().to_owned())
            .ok_or_else(|| format!("{}: no candidates", case.reading))?;
        let intended_rank = candidates
            .iter()
            .position(|candidate| candidate.text() == case.expected)
            .map(|rank| i32::try_from(rank + 1).unwrap_or(i32::MAX))
            .unwrap_or(-1);
        let steal =
            top1 != case.expected && top1.contains(&case.predator) && !top1.contains(&case.prey);
        hits.push(Hit {
            kind: case.kind,
            reading: case.reading.clone(),
            expected: case.expected.clone(),
            top1,
            intended_rank,
            predator: case.predator.clone(),
            prey: case.prey.clone(),
            gap: case.gap,
            steal,
        });
    }
    Ok(hits)
}

fn write_report(
    path: &Path,
    groups: &BTreeMap<String, Group>,
    exact_counts: &BTreeMap<String, usize>,
    hits: &[Hit],
    elapsed_s: f64,
) -> Result<(), String> {
    let mut text = String::new();
    writeln!(
        &mut text,
        "kind\treading\texpected\ttop1\tintended_rank\tpredator\tprey\tgap\tsteal\tmatch"
    )
    .map_err(fmt_error)?;
    for hit in hits {
        writeln!(
            &mut text,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            hit.kind.name(),
            hit.reading,
            hit.expected,
            hit.top1,
            hit.intended_rank,
            hit.predator,
            hit.prey,
            hit.gap,
            hit.steal,
            hit.top1 == hit.expected,
        )
        .map_err(fmt_error)?;
    }
    std::fs::write(path, text).map_err(|error| format!("write {}: {error}", path.display()))?;

    let summary_path = summary_path(path);
    let mut json = String::new();
    let steal_count = hits.iter().filter(|hit| hit.steal).count();
    let match_count = hits.iter().filter(|hit| hit.top1 == hit.expected).count();
    writeln!(&mut json, "{{").map_err(fmt_error)?;
    writeln!(&mut json, "  \"cases\": {},", hits.len()).map_err(fmt_error)?;
    writeln!(&mut json, "  \"matches\": {},", match_count).map_err(fmt_error)?;
    writeln!(&mut json, "  \"steals\": {},", steal_count).map_err(fmt_error)?;
    writeln!(
        &mut json,
        "  \"other_mismatches\": {},",
        hits.len() - match_count - steal_count
    )
    .map_err(fmt_error)?;
    writeln!(&mut json, "  \"elapsed_s\": {:.3},", elapsed_s).map_err(fmt_error)?;
    writeln!(&mut json, "  \"homophone_groups\": {},", groups.len()).map_err(fmt_error)?;
    let crowded = exact_counts.values().filter(|count| **count > 12).count();
    writeln!(
        &mut json,
        "  \"readings_with_over_12_exact_edges\": {},",
        crowded
    )
    .map_err(fmt_error)?;
    writeln!(&mut json, "  \"predators\": [").map_err(fmt_error)?;
    let predators = predator_counts(hits);
    for (index, ((predator, prey), count)) in predators.iter().take(30).enumerate() {
        let comma = if index + 1 == predators.len().min(30) {
            ""
        } else {
            ","
        };
        writeln!(
            &mut json,
            "    {{\"predator\":{},\"prey\":{},\"steals\":{}}}{comma}",
            json_string(predator),
            json_string(prey),
            count
        )
        .map_err(fmt_error)?;
    }
    writeln!(&mut json, "  ]").map_err(fmt_error)?;
    writeln!(&mut json, "}}").map_err(fmt_error)?;
    std::fs::write(&summary_path, json)
        .map_err(|error| format!("write {}: {error}", summary_path.display()))
}

fn print_summary(
    groups: &BTreeMap<String, Group>,
    exact_counts: &BTreeMap<String, usize>,
    hits: &[Hit],
    case_count: usize,
) {
    println!(
        "cases {} / homophone groups {} / crowded readings {}",
        case_count,
        groups.len(),
        exact_counts.values().filter(|count| **count > 12).count()
    );
    if let Some(group) = groups.get("きのう") {
        println!(
            "きのう: predator {} cost {} / prey {}",
            group.predator.surface,
            group.predator.cost,
            group
                .prey
                .iter()
                .map(|word| format!("{}:{}", word.surface, word.cost))
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!(
            "きのう exact edges: {}",
            exact_counts.get("きのう").copied().unwrap_or(0)
        );
    }
    let steal_count = hits.iter().filter(|hit| hit.steal).count();
    let match_count = hits.iter().filter(|hit| hit.top1 == hit.expected).count();
    println!(
        "matches {} ({:.1}%), steals {} ({:.1}%), other mismatches {}",
        match_count,
        percent(match_count, hits.len()),
        steal_count,
        percent(steal_count, hits.len()),
        hits.len() - match_count - steal_count
    );
    println!("top predator -> prey steals:");
    for ((predator, prey), count) in predator_counts(hits).into_iter().take(15) {
        println!("  {count}\t{predator} -> intended {prey}");
    }
    println!("seed and first steals:");
    for hit in hits
        .iter()
        .filter(|hit| hit.kind == Kind::Seed || hit.steal)
        .take(20)
    {
        println!(
            "  [{}] {} => {} (expected {}, rank {}){}",
            hit.kind.name(),
            hit.reading,
            hit.top1,
            hit.expected,
            hit.intended_rank,
            if hit.steal { " STEAL" } else { "" }
        );
    }
}

fn predator_counts(hits: &[Hit]) -> Vec<((String, String), usize)> {
    let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    for hit in hits.iter().filter(|hit| hit.steal) {
        *counts
            .entry((hit.predator.clone(), hit.prey.clone()))
            .or_default() += 1;
    }
    let mut ranked = counts.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    ranked
}

fn push_case(cases: &mut Vec<Case>, seen: &mut BTreeSet<(Kind, String, String)>, case: Case) {
    if case.reading.len() > sakura_proto::MAX_PREEDIT_BYTES {
        return;
    }
    if seen.insert((case.kind, case.reading.clone(), case.expected.clone())) {
        cases.push(case);
    }
}

fn reading_prefixes(reading: &str) -> Vec<&str> {
    let mut prefixes = Vec::new();
    let mut chars = 0usize;
    for (index, _) in reading.char_indices() {
        if index == 0 {
            continue;
        }
        chars += 1;
        if (2..=6).contains(&chars) {
            prefixes.push(&reading[..index]);
        }
    }
    prefixes
}

fn effective_cost(cost: i32, it: bool) -> i64 {
    let cost = i64::from(cost.max(0));
    if !it {
        return cost;
    }
    cost - (cost * IT_BIAS_PER_MILLE / 1_000).min(MAX_IT_BOOST)
}

fn is_kana_reading(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| matches!(character, '\u{3041}'..='\u{3096}' | 'ー' | 'ゝ' | 'ゞ'))
}

fn has_kanji(value: &str) -> bool {
    value
        .chars()
        .any(|character| matches!(character, '\u{4E00}'..='\u{9FFF}' | '々'))
}

fn is_ascii_digit(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn percent(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        (part as f64) * 100.0 / (total as f64)
    }
}

fn summary_path(path: &Path) -> PathBuf {
    let mut summary = path.to_path_buf();
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("tsv") => {
            summary.set_extension("summary.json");
        }
        _ => {
            summary.set_extension("summary.json");
        }
    }
    summary
}

fn json_string(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len() + 2);
    encoded.push('"');
    for character in value.chars() {
        match character {
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            '\n' => encoded.push_str("\\n"),
            '\r' => encoded.push_str("\\r"),
            '\t' => encoded.push_str("\\t"),
            character if character.is_control() => {
                let _ = write!(&mut encoded, "\\u{:04x}", character as u32);
            }
            character => encoded.push(character),
        }
    }
    encoded.push('"');
    encoded
}

fn fmt_error(_: std::fmt::Error) -> String {
    "formatting an in-memory report failed".to_owned()
}
