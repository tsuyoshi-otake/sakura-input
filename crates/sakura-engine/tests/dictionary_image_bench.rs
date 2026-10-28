//! Bounded comparison of startup image open/parse and representative conversion.
//!
//! Run with `cargo test --release -p sakura-engine --features dev-fixtures
//! --test dictionary_image_bench -- --ignored --nocapture` after setting both
//! `SAKURA_BASE_SYSTEM_DIC` and `SAKURA_SYSTEM_DIC` to existing image paths.
//! This benchmark is intentionally ignored: it reads images only and does not
//! touch installed dictionaries or per-user state.

use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use sakura_core::dictionary::Dictionary;
use sakura_core::ConversionOptions;
use sakura_engine::dictionary;
use sakura_values::FixedStr;

const SAMPLES: usize = 31;
const DETAIL_SAMPLES: usize = 101;
const DETAIL_WARMUPS: usize = 10;
const DETAIL_PREVIEW_BYTES: usize = 256;
const QUERIES: &[&str] = &["きょう", "にほんご", "へんかんこうほ", "さくらにゅうりょく"];
const DETAIL_TARGETS: &[(&str, &str)] = &[("たべない", "食べない")];

fn required_path(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("set {name} to a system.dic image"))
}

fn report(label: &str, mut values: Vec<Duration>) {
    values.sort_unstable();
    let percentile = |p: usize| values[((values.len() - 1) * p) / 100].as_secs_f64() * 1e6;
    eprintln!(
        "{label}: best={:.1}us p50={:.1}us p95={:.1}us p99={:.1}us (n={})",
        values[0].as_secs_f64() * 1e6,
        percentile(50),
        percentile(95),
        percentile(99),
        values.len()
    );
}

fn report_difference(label: &str, mut values: Vec<f64>) {
    values.sort_unstable_by(f64::total_cmp);
    let percentile = |p: usize| values[((values.len() - 1) * p) / 100];
    eprintln!(
        "{label}: p50={:.1}us p95={:.1}us (n={})",
        percentile(50),
        percentile(95),
        values.len()
    );
}

fn measure_open(path: &std::path::Path) -> Vec<Duration> {
    (0..SAMPLES)
        .map(|_| {
            let start = Instant::now();
            let service =
                dictionary::open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
            black_box(service);
            start.elapsed()
        })
        .collect()
}

fn measure_queries(path: &std::path::Path) -> Vec<Duration> {
    let service = dictionary::open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    (0..SAMPLES)
        .map(|sample| {
            let reading = QUERIES[sample % QUERIES.len()];
            let start = Instant::now();
            let count = service
                .with_conversion(reading, ConversionOptions::default(), |candidates, _| {
                    black_box(candidates.first().map(|candidate| candidate.text()));
                    candidates.len()
                })
                .expect("conversion");
            black_box(count);
            start.elapsed()
        })
        .collect()
}

#[test]
#[ignore = "paired release benchmark; requires baseline and current dictionary images"]
fn paired_conversion_images() {
    let baseline = dictionary::open(&required_path("SAKURA_BASE_SYSTEM_DIC")).unwrap();
    let current = dictionary::open(&required_path("SAKURA_SYSTEM_DIC")).unwrap();
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut by_query = vec![Vec::new(); QUERIES.len()];
    for sample in 0..440 {
        let reading = QUERIES[sample % QUERIES.len()];
        let mut elapsed = [Duration::ZERO; 2];
        let mut result = [None, None];
        for index in if (sample / QUERIES.len()).is_multiple_of(2) {
            [0, 1]
        } else {
            [1, 0]
        } {
            let service = if index == 0 { &baseline } else { &current };
            let start = Instant::now();
            let candidates = service
                .with_conversion(reading, ConversionOptions::default(), |candidates, _| {
                    (
                        candidates.len(),
                        candidates.first().map(|item| item.text().to_owned()),
                    )
                })
                .expect("conversion");
            elapsed[index] = start.elapsed();
            result[index] = Some(candidates);
        }
        assert_eq!(
            result[0], result[1],
            "candidate count and leader for {reading}"
        );
        if sample >= 40 {
            before.push(elapsed[0]);
            after.push(elapsed[1]);
            by_query[sample % QUERIES.len()].push((
                elapsed[0],
                elapsed[1],
                (sample / QUERIES.len()).is_multiple_of(2),
            ));
        }
    }
    report("paired baseline conversion", before);
    report("paired current conversion", after);
    for (reading, samples) in QUERIES.iter().zip(by_query) {
        report(
            &format!("{reading} baseline"),
            samples.iter().map(|s| s.0).collect(),
        );
        report(
            &format!("{reading} current"),
            samples.iter().map(|s| s.1).collect(),
        );
        for first in [true, false] {
            let differences = samples
                .iter()
                .filter(|s| s.2 == first)
                .map(|s| (s.1.as_secs_f64() - s.0.as_secs_f64()) * 1e6)
                .collect();
            report_difference(
                &format!("{reading} paired current-minus-baseline baseline-first={first}"),
                differences,
            );
        }
    }
}

fn find_target(
    dictionary: &Dictionary<'_>,
    targets: &[(&'static str, &'static str)],
    require_detail: bool,
) -> Option<(&'static str, &'static str, usize)> {
    for &(reading, surface) in targets {
        let mut ordinal = None;
        dictionary
            .common_prefix_search(reading, |matched| {
                let mut found_surface = String::new();
                dictionary
                    .write_surface(matched.entry, &mut found_surface)
                    .expect("surface");
                if matched.matched_bytes == reading.len() && found_surface == surface {
                    ordinal = Some(matched.entry_index);
                    false
                } else {
                    true
                }
            })
            .expect("target lookup");
        if let Some(ordinal) = ordinal {
            if !require_detail
                || dictionary
                    .detail_at(ordinal)
                    .expect("target detail lookup")
                    .is_some()
            {
                return Some((reading, surface, ordinal));
            }
        }
    }
    None
}

fn detail_sample(dictionary: &Dictionary<'_>, ordinal: usize) -> Duration {
    let start = Instant::now();
    let detail = dictionary
        .detail_at(ordinal)
        .expect("detail lookup")
        .expect("selected target must have details");
    let mut preview = FixedStr::<512>::new();
    let truncated = detail
        .write_description_preview(&mut preview, DETAIL_PREVIEW_BYTES)
        .expect("preview");
    black_box((preview.as_str(), truncated));
    start.elapsed()
}

#[test]
#[ignore = "explicit release benchmark; requires baseline and current dictionary images"]
fn compare_dictionary_images() {
    let baseline = required_path("SAKURA_BASE_SYSTEM_DIC");
    let current = required_path("SAKURA_SYSTEM_DIC");
    for (label, path) in [("baseline", baseline), ("current", current)] {
        eprintln!("{label} image: {}", path.display());
        report(&format!("{label} open+validate"), measure_open(&path));
        report(
            &format!("{label} conversion candidate search/presentation"),
            measure_queries(&path),
        );
    }
    let baseline_bytes =
        std::fs::read(required_path("SAKURA_BASE_SYSTEM_DIC")).expect("read baseline image");
    let current_bytes =
        std::fs::read(required_path("SAKURA_SYSTEM_DIC")).expect("read current image");
    let baseline_dictionary = Dictionary::parse(&baseline_bytes).expect("parse baseline image");
    let current_dictionary = Dictionary::parse(&current_bytes).expect("parse current image");
    let mut native_ordinal = None;
    current_dictionary
        .common_prefix_search("よぎり", |matched| {
            let mut surface = String::new();
            current_dictionary
                .write_surface(matched.entry, &mut surface)
                .unwrap();
            if matched.matched_bytes == "よぎり".len()
                && surface == "過ぎり"
                && current_dictionary
                    .detail_at(matched.entry_index)
                    .unwrap()
                    .is_some()
                && baseline_dictionary
                    .detail_at(matched.entry_index)
                    .unwrap()
                    .is_none()
            {
                native_ordinal = Some(matched.entry_index);
                return false;
            }
            true
        })
        .expect("native target lookup");
    let native_ordinal = native_ordinal.expect("new native 過ぎり detail");
    let mut native_description = String::new();
    current_dictionary
        .detail_at(native_ordinal)
        .unwrap()
        .unwrap()
        .write_description(&mut native_description)
        .unwrap();
    assert!(
        native_description.starts_with("「過ぎる」の活用形。基本形の意味："),
        "{native_description}"
    );
    eprintln!("new native 過ぎり detail: {native_description}");
    let current_target = find_target(&current_dictionary, DETAIL_TARGETS, true)
        .expect("known detailed target in current image");
    let requested = [(current_target.0, current_target.1)];
    let baseline_target = find_target(&baseline_dictionary, &requested, false);
    let baseline_target = baseline_target.expect(
        "selected current detail target must also have a baseline entry for ordinal resolution",
    );
    eprintln!(
        "detail targets resolved independently: baseline={} / {} ordinal={}, current={} / {} ordinal={}",
        baseline_target.0,
        baseline_target.1,
        baseline_target.2,
        current_target.0,
        current_target.1,
        current_target.2
    );
    assert_eq!(
        (baseline_target.0, baseline_target.1),
        (current_target.0, current_target.1),
        "comparison target must match"
    );
    let baseline_has_detail = baseline_dictionary
        .detail_at(baseline_target.2)
        .expect("baseline detail lookup")
        .is_some();
    let mut definition = String::new();
    current_dictionary
        .detail_at(current_target.2)
        .expect("current detail lookup")
        .expect("current detail")
        .write_description(&mut definition)
        .expect("current detail text");
    if (current_target.0, current_target.1) == ("たべない", "食べない") {
        assert!(
            definition.starts_with("「食べる」の活用形。基本形の意味："),
            "{definition}"
        );
    }
    eprintln!("current detail text: {definition}");
    eprintln!(
        "detail presence: baseline={baseline_has_detail}, current=true (current target selection requires a detail record)"
    );
    for _ in 0..DETAIL_WARMUPS {
        if baseline_has_detail {
            black_box(detail_sample(&baseline_dictionary, baseline_target.2));
        }
        black_box(detail_sample(&current_dictionary, current_target.2));
    }
    let mut baseline_detail = Vec::with_capacity(DETAIL_SAMPLES);
    let mut current_detail = Vec::with_capacity(DETAIL_SAMPLES);
    for i in 0..DETAIL_SAMPLES {
        if i % 2 == 0 {
            if baseline_has_detail {
                baseline_detail.push(detail_sample(&baseline_dictionary, baseline_target.2));
            }
            current_detail.push(detail_sample(&current_dictionary, current_target.2));
        } else {
            current_detail.push(detail_sample(&current_dictionary, current_target.2));
            if baseline_has_detail {
                baseline_detail.push(detail_sample(&baseline_dictionary, baseline_target.2));
            }
        }
    }
    if baseline_has_detail {
        report("baseline exact detail lookup+preview", baseline_detail);
    } else {
        eprintln!(
            "baseline exact detail lookup: selected ordinal has no detail record; no comparable preview timing"
        );
    }
    report("current exact detail lookup+preview", current_detail);
    let existing_target = [("かく", "書く")];
    let baseline_existing = find_target(&baseline_dictionary, &existing_target, true)
        .expect("existing baseline detail target");
    let current_existing = find_target(&current_dictionary, &existing_target, true)
        .expect("existing current detail target");
    for _ in 0..DETAIL_WARMUPS {
        black_box(detail_sample(&baseline_dictionary, baseline_existing.2));
        black_box(detail_sample(&current_dictionary, current_existing.2));
    }
    let mut baseline_existing_times = Vec::with_capacity(DETAIL_SAMPLES);
    let mut current_existing_times = Vec::with_capacity(DETAIL_SAMPLES);
    for i in 0..DETAIL_SAMPLES {
        if i % 2 == 0 {
            baseline_existing_times.push(detail_sample(&baseline_dictionary, baseline_existing.2));
            current_existing_times.push(detail_sample(&current_dictionary, current_existing.2));
        } else {
            current_existing_times.push(detail_sample(&current_dictionary, current_existing.2));
            baseline_existing_times.push(detail_sample(&baseline_dictionary, baseline_existing.2));
        }
    }
    report(
        "baseline existing 書く detail lookup+preview",
        baseline_existing_times,
    );
    report(
        "current existing 書く detail lookup+preview",
        current_existing_times,
    );
    eprintln!(
        "Limitations: open+validate includes file mapping and OS cache effects; 31 open/conversion samples and 101 warmed detail samples are local comparison sizes. Conversion measures four fixed readings and first-candidate surface access, not UI dispatch. Detail timing starts at detail_at for an independently resolved exact reading/surface ordinal and includes a 256-byte bounded description preview, but excludes image read/parse. Compare only runs on the same machine/build and record image hashes separately."
    );
}
