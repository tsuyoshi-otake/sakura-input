//! Allocation and micro-latency evidence for the dormant Issue #34 context core.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;
use std::time::Instant;

use sakura_engine::context_evaluation::{evaluate_replay, ReplayObservation};
use sakura_engine::context_intelligence::{ContextClearReason, SessionSemanticContext};
use sakura_engine::prediction_snapshot::{
    DictionaryIdentity, PredictionSnapshot, SnapshotCandidateInput, SnapshotSource,
};
use sakura_neural_proto::CandidateAuthority;
use sakura_proto::InputScope;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: every allocation operation is forwarded unchanged to `System`. The
// thread-local counter is const-initialized and does not allocate or share data.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: `layout` is forwarded unchanged to the system allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: `layout` is forwarded unchanged to the system allocator.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count();
        // SAFETY: all arguments are forwarded unchanged to `System`.
        unsafe { System.realloc(pointer, layout, size) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: all arguments are forwarded unchanged to `System`.
        unsafe { System.dealloc(pointer, layout) }
    }
}

fn count() {
    let _ = ALLOCATIONS.try_with(|cell| cell.set(cell.get().saturating_add(1)));
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn allocations(body: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.with(Cell::get);
    body();
    ALLOCATIONS.with(Cell::get) - before
}

#[test]
fn context_append_snapshot_and_clear_allocate_nothing() {
    let mut context = SessionSemanticContext::new();
    context.append_definitive_commit(InputScope::Normal, true, false, "warmup");

    let observed = allocations(|| {
        for surface in ["日本語", "context", "末尾。", "再開"] {
            black_box(context.append_definitive_commit(InputScope::Normal, true, false, surface));
            black_box(context.snapshot());
        }
        black_box(context.clear(ContextClearReason::Explicit));
    });

    assert_eq!(observed, 0);
}

#[test]
fn prediction_snapshot_and_replay_evaluation_allocate_nothing() {
    let inputs = [
        SnapshotCandidateInput {
            reading: "かな",
            surface: "仮名",
            dictionary_identity: Some(DictionaryIdentity::SystemEntry(1)),
            base_cost: 100,
            authority: CandidateAuthority::Ordinary,
            source: SnapshotSource::SystemDictionary,
            right_id: 1,
            is_it: false,
        },
        SnapshotCandidateInput {
            reading: "かな",
            surface: "かな",
            dictionary_identity: None,
            base_cost: 50,
            authority: CandidateAuthority::ExactLearning,
            source: SnapshotSource::History,
            right_id: 1,
            is_it: false,
        },
    ];
    let snapshot = PredictionSnapshot::build(1, 1, 1, InputScope::Normal, true, false, &inputs)
        .expect("warm snapshot");
    let ranked = [
        snapshot.candidates()[1].candidate_id,
        snapshot.candidates()[0].candidate_id,
    ];

    let observed = allocations(|| {
        let snapshot = black_box(
            PredictionSnapshot::build(1, 1, 1, InputScope::Normal, true, false, black_box(&inputs))
                .expect("valid snapshot"),
        );
        black_box(
            evaluate_replay(&[ReplayObservation {
                snapshot: &snapshot,
                ranked_candidate_ids: black_box(&ranked),
                expected_candidate_id: Some(ranked[0]),
                response_correlation: snapshot.correlation(),
                keystrokes_without_prediction: 4,
                keystrokes_with_prediction: 2,
            }])
            .expect("valid replay"),
        );
    });

    assert_eq!(observed, 0);
}

/// Fixture-only timing evidence. This is not an acceptance or production p99.
#[test]
#[ignore = "release-only context append microbenchmark"]
fn context_append_release_percentiles() {
    const SAMPLES: usize = 10_000;
    let mut samples = [0u64; SAMPLES];
    let mut context = SessionSemanticContext::new();
    for _ in 0..1_000 {
        black_box(context.append_definitive_commit(
            InputScope::Normal,
            true,
            false,
            "日本語context。",
        ));
    }

    for elapsed in &mut samples {
        let start = Instant::now();
        black_box(context.append_definitive_commit(
            InputScope::Normal,
            true,
            false,
            "日本語context。",
        ));
        *elapsed = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
    }
    samples.sort_unstable();
    println!(
        "context append fixture: p50={}ns p95={}ns p99={}ns max={}ns samples={SAMPLES}",
        samples[SAMPLES / 2],
        samples[SAMPLES * 95 / 100],
        samples[SAMPLES * 99 / 100],
        samples[SAMPLES - 1]
    );
}
