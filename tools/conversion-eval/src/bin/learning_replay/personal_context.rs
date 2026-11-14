//! Offline, count-only evaluation of the #477 personal-context methods.

use std::collections::{BTreeMap, BTreeSet};

use sakura_core::ConversionCandidate;
use sakura_engine::committed_context::CommittedContext;

use super::{CommitRow, Event, HistoryRow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    S,
    P,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Config {
    method: Method,
    n: usize,
    k: u64,
    q: u64,
}

#[derive(Debug, Clone, Copy)]
pub enum Mode {
    Baseline,
    Grid,
    Confirm(Config),
}

pub fn parse_mode(
    mode: Option<String>,
    method: Option<String>,
    n: Option<String>,
    k: Option<String>,
    q: Option<String>,
) -> Result<Option<Mode>, String> {
    let no_config = method.is_none() && n.is_none() && k.is_none() && q.is_none();
    match mode.as_deref() {
        None if no_config => Ok(None),
        Some("baseline") if no_config => Ok(Some(Mode::Baseline)),
        Some("grid") if no_config => Ok(Some(Mode::Grid)),
        Some("confirm") => {
            let method = match method.as_deref() {
                Some("S") => Method::S,
                Some("P") => Method::P,
                _ => return Err("confirmation requires --method S or P".to_owned()),
            };
            let n = n
                .and_then(|v| v.parse().ok())
                .filter(|v| [2, 3].contains(v))
                .ok_or("confirmation requires --n 2 or 3")?;
            let (k, q) = if method == Method::P {
                let k = k
                    .and_then(|v| v.parse().ok())
                    .filter(|v| [2, 3, 5].contains(v))
                    .ok_or("P requires --k 2, 3 or 5")?;
                let q = q
                    .and_then(|v| v.parse().ok())
                    .filter(|v| [60, 80, 90].contains(v))
                    .ok_or("P requires --q 60, 80 or 90")?;
                (k, q)
            } else {
                if k.is_some() || q.is_some() {
                    return Err("S does not use --k or --q".to_owned());
                }
                (0, 0)
            };
            Ok(Some(Mode::Confirm(Config { method, n, k, q })))
        }
        _ => Err("invalid personal mode or configuration".to_owned()),
    }
}

fn grid() -> Vec<Config> {
    let mut configs = Vec::new();
    for n in [2, 3] {
        configs.push(Config {
            method: Method::S,
            n,
            k: 0,
            q: 0,
        });
        for k in [2, 3, 5] {
            for q in [60, 80, 90] {
                configs.push(Config {
                    method: Method::P,
                    n,
                    k,
                    q,
                });
            }
        }
    }
    configs
}

fn character_class(c: char) -> u8 {
    match c {
        '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}' | '々' => 1,
        '\u{30a1}'..='\u{30fa}' | 'ー' => 2,
        c if c.is_ascii_alphanumeric() => 3,
        _ => 0,
    }
}

fn clues(tail: &str) -> Vec<&str> {
    let mut spans = Vec::new();
    let mut start = 0;
    let mut class = 0;
    for (index, c) in tail.char_indices() {
        let next = character_class(c);
        if next != class {
            if class != 0 {
                spans.push(&tail[start..index]);
            }
            start = index;
            class = next;
        }
    }
    if class != 0 {
        spans.push(&tail[start..]);
    }
    let mut seen = BTreeSet::new();
    spans
        .into_iter()
        .rev()
        .filter(|word| word.chars().count() >= 2 && seen.insert(*word))
        .take(16)
        .collect()
}

#[derive(Default)]
struct Evidence {
    count: u32,
    last_day: u32,
}

impl Evidence {
    fn effective(&self, day: u32) -> u64 {
        u64::from(
            self.count
                .checked_shr(day.saturating_sub(self.last_day) / 30)
                .unwrap_or(0),
        )
    }

    fn add(&mut self, day: u32) {
        // Match #305: increment the raw frequency and update its last-seen day.
        self.count = self.count.saturating_add(1);
        self.last_day = day;
    }
}

#[derive(Default)]
struct PersonalCounts {
    // Includes spellings outside the current candidate list for the denominator.
    readings: BTreeMap<String, BTreeMap<String, BTreeMap<String, Evidence>>>,
}

impl PersonalCounts {
    fn learn(&mut self, day: u32, reading: &str, surface: &str, words: &[&str]) {
        for word in words {
            self.readings
                .entry(reading.to_owned())
                .or_default()
                .entry(surface.to_owned())
                .or_default()
                .entry((*word).to_owned())
                .or_default()
                .add(day);
        }
    }

    fn winner<'a>(
        &'a self,
        day: u32,
        reading: &str,
        words: &[&str],
    ) -> Option<(&'a str, u64, u64)> {
        let surfaces = self.readings.get(reading)?;
        let mut total = 0u64;
        let mut best = None;
        for (surface, counts) in surfaces {
            let score: u64 = words
                .iter()
                .filter_map(|word| counts.get(*word))
                .map(|entry| entry.effective(day))
                .sum();
            total += score;
            // BTreeMap order breaks equal scores deterministically by surface.
            if score > 0 && best.is_none_or(|(_, previous)| score > previous) {
                best = Some((surface.as_str(), score));
            }
        }
        best.map(|(surface, score)| (surface, score, total))
    }
}

fn choose<'a>(
    config: Config,
    day: u32,
    reading: &str,
    tail: &str,
    candidates: &[&'a str],
    counts: &PersonalCounts,
) -> &'a str {
    let first = candidates[0];
    if tail.is_empty() {
        return first;
    }
    match config.method {
        Method::S => {
            if tail.contains(first) {
                return first;
            }
            candidates
                .iter()
                .take(config.n)
                .filter(|surface| {
                    **surface != reading
                        && surface.chars().any(|c| {
                            character_class(c) == 1 || matches!(c, '\u{30a1}'..='\u{30fa}')
                        })
                })
                .filter_map(|surface| tail.rfind(surface).map(|position| (*surface, position)))
                .max_by_key(|(_, position)| *position)
                .map_or(first, |(surface, _)| surface)
        }
        Method::P => {
            let Some((winner, score, total)) = counts.winner(day, reading, &clues(tail)) else {
                return first;
            };
            if score < config.k || score * 100 < config.q * total {
                return first;
            }
            candidates
                .iter()
                .take(config.n)
                .copied()
                .find(|surface| *surface == winner)
                .unwrap_or(first)
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Counts {
    target: usize,
    baseline_match: usize,
    changed: usize,
    fixed: usize,
    broken: usize,
    short_target: usize,
    short_changed: usize,
    short_fixed: usize,
    short_broken: usize,
}

impl Counts {
    fn add(&mut self, first: &str, selected: &str, committed: &str, short: bool) {
        self.target += 1;
        self.baseline_match += usize::from(first == committed);
        self.changed += usize::from(first != selected);
        self.fixed += usize::from(first != committed && selected == committed);
        self.broken += usize::from(first == committed && selected != committed);
        if short {
            self.short_target += 1;
            self.short_changed += usize::from(first != selected);
            self.short_fixed += usize::from(first != committed && selected == committed);
            self.short_broken += usize::from(first == committed && selected != committed);
        }
    }
}

pub struct Measurement {
    mode: Mode,
    cutoff: u32,
    last_day: u32,
    session: Option<u64>,
    tail: CommittedContext,
    personal: PersonalCounts,
    configs: Vec<Config>,
    counts: Vec<Counts>,
    baseline: Counts,
    empty: usize,
    empty_changed: usize,
}

impl Measurement {
    pub fn new(mode: Mode, rows: &[HistoryRow]) -> Self {
        let first = rows.iter().map(|row| row.day).min().unwrap_or(0);
        let last_day = rows.iter().map(|row| row.day).max().unwrap_or(first);
        let cutoff = first + (last_day - first) / 2;
        let configs = match mode {
            Mode::Baseline => Vec::new(),
            Mode::Grid => grid(),
            Mode::Confirm(config) => vec![config],
        };
        Self {
            mode,
            cutoff,
            last_day,
            session: None,
            tail: CommittedContext::new(),
            personal: PersonalCounts::default(),
            counts: vec![Counts::default(); configs.len()],
            configs,
            baseline: Counts::default(),
            empty: 0,
            empty_changed: 0,
        }
    }

    pub fn should_stop(&self, day: u32) -> bool {
        matches!(self.mode, Mode::Grid) && day > self.cutoff
    }

    pub fn begin_row(&mut self, row: &HistoryRow) {
        if !matches!(row.event, Event::Key(_) | Event::Commit(_)) {
            return;
        }
        if self.session != Some(row.session) {
            self.tail.clear();
            self.session = Some(row.session);
        }
        if row.scope != "normal" {
            self.tail.clear();
        }
    }

    fn scoring(&self, day: u32) -> bool {
        match self.mode {
            Mode::Baseline | Mode::Grid => true,
            Mode::Confirm(_) => day > self.cutoff,
        }
    }

    pub fn score(
        &mut self,
        row: &HistoryRow,
        commit: &CommitRow,
        candidates: &[ConversionCandidate],
        index: usize,
    ) {
        if row.scope != "normal" || !self.scoring(row.day) {
            return;
        }
        // #305 chooses an index rather than reordering its candidate list.
        // Promote that choice to the front, keeping all other rows in order.
        let mut ordered = vec![candidates[index].text()];
        for (i, candidate) in candidates.iter().enumerate() {
            if i != index && !ordered.contains(&candidate.text()) {
                ordered.push(candidate.text());
                if ordered.len() == 3 {
                    break;
                }
            }
        }
        let first = ordered[0];
        let short = commit.reading.chars().count() <= 4;
        if self.tail.as_str().is_empty() {
            self.empty += 1;
            for config in &self.configs {
                let selected = choose(
                    *config,
                    row.day,
                    &commit.reading,
                    "",
                    &ordered,
                    &self.personal,
                );
                self.empty_changed += usize::from(selected != first);
            }
            return;
        }
        self.baseline.add(first, first, &commit.surface, short);
        for (config, counts) in self.configs.iter().zip(&mut self.counts) {
            let selected = choose(
                *config,
                row.day,
                &commit.reading,
                self.tail.as_str(),
                &ordered,
                &self.personal,
            );
            counts.add(first, selected, &commit.surface, short);
        }
    }

    pub fn commit(&mut self, row: &HistoryRow, commit: &CommitRow, converted: bool) {
        if row.scope != "normal" {
            self.tail.clear();
            return;
        }
        if converted && commit.reading != commit.surface {
            self.personal.learn(
                row.day,
                &commit.reading,
                &commit.surface,
                &clues(self.tail.as_str()),
            );
        }
        self.tail.append(&commit.surface);
    }

    pub fn report(&self) -> String {
        let mut out = format!(
            "cutoff-day\t{}\nlast-day\t{}\nempty-target\t{}\nempty-differences\t{}\nbaseline-target\t{}\nbaseline-match\t{}\n",
            self.cutoff, self.last_day, self.empty, self.empty_changed, self.baseline.target, self.baseline.baseline_match
        );
        out.push_str("method\tn\tk\tq-percent\ttarget\tbaseline-match\tfixed\tbroken\tchanged\tshort-target\tshort-fixed\tshort-broken\tshort-changed\n");
        for (config, count) in self.configs.iter().zip(&self.counts) {
            out.push_str(&format!(
                "{:?}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                config.method,
                config.n,
                config.k,
                config.q,
                count.target,
                count.baseline_match,
                count.fixed,
                count.broken,
                count.changed,
                count.short_target,
                count.short_fixed,
                count.short_broken,
                count.short_changed
            ));
        }
        if matches!(self.mode, Mode::Grid) {
            let best = self
                .configs
                .iter()
                .zip(&self.counts)
                .filter(|(_, count)| count.fixed >= 5 * count.broken)
                .max_by_key(|(config, count)| {
                    (
                        count.fixed as i64 - count.broken as i64,
                        usize::from(config.method == Method::S),
                        std::cmp::Reverse(config.n),
                        config.k,
                        config.q,
                    )
                });
            if let Some((config, count)) = best {
                out.push_str(&format!(
                    "selected\t{:?}\t{}\t{}\t{}\t{}\t{}\n",
                    config.method, config.n, config.k, config.q, count.fixed, count.broken
                ));
            } else {
                out.push_str("selected\tno-improvement\n");
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clues_are_recent_distinct_and_classified() {
        assert_eq!(
            clues("制度とカタカナ ABC12 制度 and x"),
            vec!["and", "制度", "ABC12", "カタカナ"]
        );
    }

    #[test]
    fn s_uses_most_recent_eligible_surface_and_never_changes_empty_tail() {
        let config = Config {
            method: Method::S,
            n: 3,
            k: 0,
            q: 0,
        };
        let counts = PersonalCounts::default();
        let candidates = ["仮名", "加奈", "カナ"];
        assert_eq!(choose(config, 1, "かな", "", &candidates, &counts), "仮名");
        assert_eq!(
            choose(config, 1, "かな", "加奈とカナ", &candidates, &counts),
            "カナ"
        );
        assert_eq!(
            choose(config, 1, "かな", "仮名と加奈", &candidates, &counts),
            "仮名"
        );
        assert_eq!(
            choose(config, 1, "かな", "ー", &["かな", "ー", "仮名"], &counts),
            "かな"
        );
    }

    #[test]
    fn p_counts_other_candidates_and_only_learns_after_scoring() {
        let config = Config {
            method: Method::P,
            n: 2,
            k: 2,
            q: 80,
        };
        let mut counts = PersonalCounts::default();
        let candidates = ["仮名", "加奈"];
        counts.learn(1, "かな", "加奈", &["制度"]);
        assert_eq!(
            choose(config, 1, "かな", "制度", &candidates, &counts),
            "仮名"
        );
        counts.learn(1, "かな", "加奈", &["制度"]);
        assert_eq!(
            choose(config, 1, "かな", "制度", &candidates, &counts),
            "加奈"
        );
        counts.learn(1, "かな", "別表記", &["制度"]);
        assert_eq!(
            choose(config, 1, "かな", "制度", &candidates, &counts),
            "仮名"
        );
        assert_eq!(counts.winner(31, "かな", &["制度"]), Some(("加奈", 1, 1)));
    }

    #[test]
    fn grid_has_exactly_twenty_distinct_points() {
        assert_eq!(grid().len(), 20);
    }

    fn commit_row(day: u32, session: u64, scope: &str, reading: &str, surface: &str) -> HistoryRow {
        HistoryRow {
            sequence: u64::from(day),
            day,
            session,
            scope: scope.to_owned(),
            event: Event::Commit(CommitRow {
                reading: reading.to_owned(),
                surface: surface.to_owned(),
                left_context: 0,
                right_context: 0,
            }),
        }
    }

    fn feed(measurement: &mut Measurement, row: &HistoryRow, converted: bool) {
        measurement.begin_row(row);
        if let Event::Commit(commit) = &row.event {
            measurement.commit(row, commit, converted);
        }
    }

    #[test]
    fn privacy_session_reset_and_post_commit_learning() {
        let rows = [
            commit_row(1, 1, "normal", "せいど", "制度"),
            commit_row(1, 1, "normal", "かな", "加奈"),
            commit_row(1, 1, "sensitive", "ひみつ", "秘密"),
            commit_row(1, 1, "normal", "かな", "仮名"),
            commit_row(2, 2, "normal", "かな", "加奈"),
        ];
        let mut measurement = Measurement::new(Mode::Grid, &rows);
        feed(&mut measurement, &rows[0], false);
        assert!(measurement.personal.winner(1, "かな", &["制度"]).is_none());
        feed(&mut measurement, &rows[1], true);
        assert_eq!(
            measurement.personal.winner(1, "かな", &["制度"]),
            Some(("加奈", 1, 1))
        );
        feed(&mut measurement, &rows[2], true);
        assert!(measurement.tail.as_str().is_empty());
        assert!(measurement
            .personal
            .winner(1, "ひみつ", &["制度"])
            .is_none());
        feed(&mut measurement, &rows[3], true);
        assert!(measurement.personal.winner(1, "かな", &["秘密"]).is_none());
        measurement.begin_row(&rows[4]);
        assert!(measurement.tail.as_str().is_empty());
        assert!(measurement.should_stop(2));
    }
}
