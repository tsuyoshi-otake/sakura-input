//! Measurement-only probe for sakura-input Issue #474 completion condition
//! 11: run the shipped Sakura-Rerank-Tiny model over `assoc-eval dump`
//! candidates in three input shapes —
//!   A: product-identical (context and reading tensors stay zero),
//!   B: reading tensor filled,
//!   C: reading + committed-clue context tensors filled.
//!
//! The probe shares the worker's own `prepare_inputs`/`character_id`/`text_ids`
//! through `#[path]` includes, so the char->id mapping is the shipped code,
//! not a copy (identity is also pinned by `text_ids_matches_the_candidate_
//! encoding`). The shipped `sakura_neural_worker` binary is untouched: this
//! bin builds its own ORT session because `ModelRuntime::scores` correctly
//! rejects non-zero context/reading under protocol v1.
//!
//! Output is aggregate counts only (no raw surfaces) — the same units the
//! association-table evaluation reports.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[path = "../protocol.rs"]
#[allow(dead_code)] // the probe uses only Candidate/Request construction
mod protocol;
#[path = "../sakura_runtime.rs"]
#[allow(dead_code)] // the probe reuses constants + sibling_dll only
mod sakura_runtime;
#[path = "../sakura_scorer.rs"]
#[allow(dead_code)] // the probe reuses prepare_inputs/character_id/text_ids only
mod sakura_scorer;

use protocol::{Candidate, Request};
use sakura_runtime::Inputs;

/// The shipped margin (`sakura_rerank_proto::Selection::LOCAL_OVERRIDE_MARGIN`
/// is 1.7); kept local so this crate adds no new dependency edge.
const MARGIN: f64 = 1.7;
const TOP_K: usize = 6;

#[derive(Debug, serde::Deserialize)]
struct Row {
    #[allow(dead_code)] // carried in the dump for traceability; output stays aggregate
    id: String,
    pair: String,
    expected: String,
    variants: Vec<String>,
    reading: String,
    #[allow(dead_code)] // carried in the dump for the verifier's reuse
    oneshot_expected: String,
    clue_surface: String,
    candidates: Vec<DumpCandidate>,
}

#[derive(Debug, serde::Deserialize)]
struct DumpCandidate {
    text: String,
    cost: i64,
    neural_group: Option<u8>,
}

#[derive(Clone, Copy, Default)]
struct Tally {
    rows: usize,
    strict: usize,
    lenient: usize,
    fixed: usize,
    broken: usize,
    unavailable: usize,
    skipped: usize,
    p50_us: u64,
    p99_us: u64,
    pairs_both: usize,
    pairs_one: usize,
    pairs_neither: usize,
}

/// Encodes one text into a bounded id tensor + length, exactly as the
/// research export does: prefix truncation at `capacity`, hash ids, zero
/// padding. Lives in the probe (never the shipped binary); the identity
/// test below pins it to `character_id`, which `prepare_inputs` also uses.
fn text_ids(text: &str, capacity: usize) -> (Vec<i64>, i64) {
    let mut ids = vec![0i64; capacity];
    let mut length = 0usize;
    for (slot, character) in ids.iter_mut().zip(text.chars().take(capacity)) {
        *slot = i64::from(sakura_scorer::character_id(character));
        length += 1;
    }
    (ids, length as i64)
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|at| args.get(at + 1).cloned())
}

/// First candidate that is not the raw reading — mirrors the tool's
/// `local_choice_index` and the gate's local choice.
fn local_choice_index(candidates: &[DumpCandidate], reading: &str) -> usize {
    candidates
        .iter()
        .position(|candidate| candidate.text != reading)
        .unwrap_or(0)
}

/// Mirrors `neural_listwise_choice` (sakura-core conversion/evidence.rs):
/// the highest neural evidence group wins, argmax log-probability inside it;
/// None when fewer than 2 candidates, no group, or a non-finite score.
fn listwise_choice(candidates: &[DumpCandidate], scores: &[f32]) -> Option<usize> {
    if candidates.len() < 2 {
        return None;
    }
    let group = candidates.iter().filter_map(|c| c.neural_group).max()?;
    let mut selected = None;
    let mut best = f32::NEG_INFINITY;
    for (index, candidate) in candidates.iter().enumerate() {
        if candidate.neural_group != Some(group) {
            continue;
        }
        let score = scores[index];
        if !score.is_finite() {
            return None;
        }
        if selected.is_none() || score > best {
            selected = Some(index);
            best = score;
        }
    }
    selected
}

/// Mirrors `neural_gated_choice`: override the local choice only when the
/// listwise winner's score clears the local candidate's score by > margin.
/// Returns the index the engine would show.
fn gated_choice(candidates: &[DumpCandidate], scores: &[f32], local: usize, gated: bool) -> usize {
    let Some(winner) = listwise_choice(candidates, scores) else {
        return local;
    };
    if !gated || winner == local {
        return winner;
    }
    let winner_score = f64::from(scores[winner]);
    let local_score = f64::from(scores[local]);
    if winner_score - local_score > MARGIN {
        winner
    } else {
        local
    }
}

fn percentile(samples: &[Duration], numerator: usize, denominator: usize) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let index = (samples.len() * numerator / denominator).min(samples.len() - 1);
    samples[index].as_micros() as u64
}

fn state_of(top: &str, expected: &str, variants: &[String]) -> u8 {
    if top == expected {
        2
    } else if variants.iter().any(|variant| variant == top) {
        1
    } else {
        0
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let (Some(candidates_path), Some(model_path)) = (
        arg_value(&args, "--candidates").map(PathBuf::from),
        arg_value(&args, "--model").map(PathBuf::from),
    ) else {
        eprintln!(
            "usage: context-probe --candidates <dump.jsonl> --model <model.onnx> [--model2 <model.onnx>]"
        );
        std::process::exit(2);
    };
    let model2_path = arg_value(&args, "--model2").map(PathBuf::from);

    let model_bytes = fs::read(&model_path).expect("model read");
    let models: Vec<(String, Vec<u8>)> =
        std::iter::once((model_path.display().to_string(), model_bytes))
            .chain(
                model2_path
                    .iter()
                    .map(|p| (p.display().to_string(), fs::read(p).expect("model2 read"))),
            )
            .collect();

    let text = fs::read_to_string(&candidates_path).expect("candidates read");
    let rows: Vec<Row> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("row json"))
        .collect();

    for (name, bytes) in &models {
        run_model(name, bytes, &rows);
    }
}

fn run_model(name: &str, model_bytes: &[u8], rows: &[Row]) {
    let executable = env::current_exe().expect("exe");
    let dll = sakura_runtime::sibling_dll(&executable).expect("onnxruntime.dll sibling");
    ort::init_from(dll).expect("ort init").commit();
    let mut session = ort::session::Session::builder()
        .expect("session builder")
        .with_intra_threads(1)
        .expect("threads")
        .with_inter_threads(1)
        .expect("threads")
        .with_parallel_execution(false)
        .expect("parallel")
        .commit_from_memory(model_bytes)
        .expect("session");

    println!("model: {name}");
    for gated in [true, false] {
        for pass in ["A", "B", "C"] {
            let mut tally = Tally::default();
            let mut latencies = Vec::with_capacity(rows.len() * 2);
            let mut pair_strict: std::collections::BTreeMap<String, usize> =
                std::collections::BTreeMap::new();
            for row in rows {
                if row.candidates.len() < 2 || row.candidates.len() > TOP_K {
                    tally.skipped += 1;
                    continue;
                }
                let request = Request {
                    id: 1,
                    candidates: row
                        .candidates
                        .iter()
                        .enumerate()
                        .map(|(index, candidate)| Candidate {
                            fingerprint: index as u64,
                            local_cost: candidate.cost.clamp(i32::MIN as i64, i32::MAX as i64)
                                as i32,
                            text: candidate.text.clone(),
                        })
                        .collect(),
                };
                let Ok(mut inputs) = sakura_scorer::prepare_inputs(&request) else {
                    tally.skipped += 1;
                    continue;
                };
                match pass {
                    "B" | "C" => {
                        let (ids, len) =
                            text_ids(&row.reading, sakura_runtime::READING_LENGTH);
                        inputs.reading_ids = ids;
                        inputs.reading_lengths = vec![len];
                    }
                    _ => {}
                }
                if pass == "C" {
                    let (ids, len) =
                        text_ids(&row.clue_surface, sakura_runtime::CONTEXT_LENGTH);
                    inputs.context_ids = ids;
                    inputs.context_lengths = vec![len];
                }
                let start = Instant::now();
                let scores = score_once(&mut session, &inputs);
                latencies.push(start.elapsed());
                let Some(scores) = scores else {
                    tally.unavailable += 1;
                    continue;
                };
                let local = local_choice_index(&row.candidates, &row.reading);
                let chosen = gated_choice(&row.candidates, &scores, local, gated);
                let top = &row.candidates[chosen].text;
                let baseline_state =
                    state_of(&row.candidates[local].text, &row.expected, &row.variants);
                let state = state_of(top, &row.expected, &row.variants);
                tally.rows += 1;
                if state == 2 {
                    tally.strict += 1;
                    *pair_strict.entry(row.pair.clone()).or_insert(0) += 1;
                } else {
                    pair_strict.entry(row.pair.clone()).or_insert(0);
                }
                if state >= 1 {
                    tally.lenient += 1;
                }
                if baseline_state == 0 && state >= 1 {
                    tally.fixed += 1;
                }
                if baseline_state >= 1 && state == 0 {
                    tally.broken += 1;
                }
            }
            latencies.sort();
            tally.p50_us = percentile(&latencies, 50, 100);
            tally.p99_us = percentile(&latencies, 99, 100);
            tally.pairs_both = pair_strict.values().filter(|&&n| n == 2).count();
            tally.pairs_one = pair_strict.values().filter(|&&n| n == 1).count();
            tally.pairs_neither = pair_strict.values().filter(|&&n| n == 0).count();
            println!(
                "pass={pass} gate={} rows={} strict={} lenient={} \
                 pairs_both={} pairs_one={} pairs_neither={} \
                 fixed={} broken={} unavailable={} skipped={} \
                 p50_us={} p99_us={}",
                if gated { "1.7" } else { "off" },
                tally.rows,
                tally.strict,
                tally.lenient,
                tally.pairs_both,
                tally.pairs_one,
                tally.pairs_neither,
                tally.fixed,
                tally.broken,
                tally.unavailable,
                tally.skipped,
                tally.p50_us,
                tally.p99_us,
            );
        }
    }
}

fn score_once(session: &mut ort::session::Session, inputs: &Inputs) -> Option<Vec<f32>> {
    let context_ids = ort::value::Tensor::from_array((
        [1, sakura_runtime::CONTEXT_LENGTH],
        inputs.context_ids.clone(),
    ))
    .ok()?;
    let context_lengths =
        ort::value::Tensor::from_array(([1], inputs.context_lengths.clone())).ok()?;
    let reading_ids = ort::value::Tensor::from_array((
        [1, sakura_runtime::READING_LENGTH],
        inputs.reading_ids.clone(),
    ))
    .ok()?;
    let reading_lengths =
        ort::value::Tensor::from_array(([1], inputs.reading_lengths.clone())).ok()?;
    let candidate_ids = ort::value::Tensor::from_array((
        [1, sakura_runtime::TOP_K, sakura_runtime::SURFACE_LENGTH],
        inputs.candidate_ids.clone(),
    ))
    .ok()?;
    let candidate_lengths = ort::value::Tensor::from_array((
        [1, sakura_runtime::TOP_K],
        inputs.candidate_lengths.clone(),
    ))
    .ok()?;
    let features = ort::value::Tensor::from_array((
        [1, sakura_runtime::TOP_K, sakura_runtime::FEATURE_DIM],
        inputs.features.clone(),
    ))
    .ok()?;
    let candidate_mask =
        ort::value::Tensor::from_array(([1, sakura_runtime::TOP_K], inputs.candidate_mask.clone()))
            .ok()?;
    let outputs = session
        .run(ort::inputs![
            "context_ids" => context_ids,
            "context_lengths" => context_lengths,
            "reading_ids" => reading_ids,
            "reading_lengths" => reading_lengths,
            "candidate_ids" => candidate_ids,
            "candidate_lengths" => candidate_lengths,
            "features" => features,
            "candidate_mask" => candidate_mask,
        ])
        .ok()?;
    let output = outputs.get("scores")?;
    let (_, values) = output.try_extract_tensor::<f32>().ok()?;
    if values.iter().any(|value| !value.is_finite()) {
        return None;
    }
    Some(values.to_vec())
}

#[cfg(test)]
mod tests {
    //! #474 condition 11: the probe's text encoder must produce the same
    //! ids the shipped candidate loop writes for the same characters —
    //! prefix truncation, hash order, zero padding.
    use super::*;

    #[test]
    fn text_ids_matches_the_candidate_encoding() {
        let text = "読みの文脈😀";
        let (ids, length) = text_ids(text, sakura_runtime::SURFACE_LENGTH);
        assert_eq!(length, text.chars().count() as i64);
        for (slot, character) in ids.iter().zip(text.chars()) {
            assert_eq!(*slot, i64::from(sakura_scorer::character_id(character)));
        }
        assert!(ids[text.chars().count()..].iter().all(|id| *id == 0));
        let (truncated, trunc_length) = text_ids(text, 2);
        assert_eq!(trunc_length, 2);
        assert_eq!(
            truncated,
            vec![
                i64::from(sakura_scorer::character_id('読')),
                i64::from(sakura_scorer::character_id('み')),
            ]
        );
    }
}
