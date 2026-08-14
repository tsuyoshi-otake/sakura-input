use std::collections::VecDeque;

use crate::engine_recovery::{
    EngineRecoveryFence, RecoveryFinish, RecoveryKeyDisposition, RecoveryStart, RecoveryTerminal,
    RecoveryToken,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OracleDisposition {
    Host,
    Consume,
}

/// Declarative test oracle. It intentionally does not call or mirror the
/// production methods: one outstanding generation fences keys; only that
/// generation's first terminal event releases the fence.
fn oracle_disposition(
    pending_generation: Option<u64>,
    callback_generation: u64,
) -> OracleDisposition {
    if pending_generation == Some(callback_generation) {
        OracleDisposition::Consume
    } else {
        OracleDisposition::Host
    }
}

fn oracle_finish(pending_generation: &mut Option<u64>, callback_generation: u64) -> bool {
    match *pending_generation {
        Some(current) if current == callback_generation => {
            *pending_generation = None;
            true
        }
        Some(_) | None => false,
    }
}

#[test]
fn recovery_fence_examples_follow_the_independent_oracle() {
    let mut fence = EngineRecoveryFence::default();
    let started = fence.begin();
    assert!(matches!(started, RecoveryStart::Started(_)));
    let first = started.token();
    assert_eq!(
        fence.disposition_after_request(first),
        RecoveryKeyDisposition::Consume
    );
    assert_eq!(
        oracle_disposition(Some(first.id()), first.id()),
        OracleDisposition::Consume
    );
    assert!(matches!(
        fence.begin(),
        RecoveryStart::Deduplicated(token) if token == first
    ));
    assert!(matches!(
        fence.finish(first, RecoveryTerminal::Applied),
        RecoveryFinish::Finished(record)
            if record.token == first && record.outcome == RecoveryTerminal::Applied
    ));
    assert_eq!(
        fence.disposition_after_request(first),
        RecoveryKeyDisposition::Host
    );
    assert_eq!(
        fence.finish(first, RecoveryTerminal::Applied),
        RecoveryFinish::IgnoredStale
    );
}

#[test]
fn reversed_old_completion_cannot_release_a_new_recovery() {
    let mut fence = EngineRecoveryFence::default();
    let old = fence.begin().token();
    assert!(fence.cancel_pending().is_some());
    let new = fence.begin().token();
    assert_ne!(old, new);

    assert_eq!(
        fence.finish(old, RecoveryTerminal::Applied),
        RecoveryFinish::IgnoredStale
    );
    assert!(fence.owns(new));
    assert!(matches!(
        fence.finish(new, RecoveryTerminal::Rejected),
        RecoveryFinish::Finished(record)
            if record.outcome == RecoveryTerminal::Rejected
    ));
    assert!(!fence.is_pending());
}

#[derive(Debug, Clone)]
struct PendingFinalizer {
    token: RecoveryToken,
    visible: String,
}

#[derive(Debug, Clone, Copy)]
enum FakeRequestMode {
    Synchronous,
    Queued,
    Rejected,
}

#[derive(Debug, Default)]
struct FakeDocumentApi {
    text: String,
    writes: usize,
}

impl FakeDocumentApi {
    fn replace(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
        self.writes += 1;
    }
}

#[derive(Debug)]
struct RecoveryApiHarness {
    fence: EngineRecoveryFence,
    document: FakeDocumentApi,
    queued: VecDeque<PendingFinalizer>,
    queue_capacity: usize,
}

impl Default for RecoveryApiHarness {
    fn default() -> Self {
        Self::with_capacity(2)
    }
}

impl RecoveryApiHarness {
    fn with_capacity(queue_capacity: usize) -> Self {
        Self {
            fence: EngineRecoveryFence::default(),
            document: FakeDocumentApi::default(),
            queued: VecDeque::new(),
            queue_capacity,
        }
    }

    fn engine_timeout(
        &mut self,
        visible: &str,
        has_composition: bool,
        request: FakeRequestMode,
    ) -> RecoveryKeyDisposition {
        if visible.is_empty() && !has_composition {
            return RecoveryKeyDisposition::Host;
        }
        let start = self.fence.begin();
        let token = start.token();
        if start.is_deduplicated() {
            return RecoveryKeyDisposition::Consume;
        }
        match request {
            FakeRequestMode::Synchronous => {
                self.document.replace(visible);
                let _ = self.fence.finish(token, RecoveryTerminal::Applied);
            }
            FakeRequestMode::Queued => {
                if self.queued.len() >= self.queue_capacity {
                    let _ = self.fence.finish(token, RecoveryTerminal::Rejected);
                } else {
                    self.queued.push_back(PendingFinalizer {
                        token,
                        visible: visible.to_owned(),
                    });
                }
            }
            FakeRequestMode::Rejected => {
                let _ = self.fence.finish(token, RecoveryTerminal::Rejected);
            }
        }
        self.fence.disposition_after_request(token)
    }

    fn key_delete(&mut self) -> RecoveryKeyDisposition {
        if self.fence.is_pending() {
            return RecoveryKeyDisposition::Consume;
        }
        self.document.replace("");
        RecoveryKeyDisposition::Host
    }

    fn external_change_and_notify(&mut self, text: &str) {
        self.document.replace(text);
        let _ = self.fence.cancel_pending();
    }

    fn deliver(&mut self, index: usize) -> bool {
        let Some(pending) = self.queued.remove(index) else {
            return false;
        };
        if !self.fence.owns(pending.token) {
            return false;
        }
        self.document.replace(&pending.visible);
        let _ = self.fence.finish(pending.token, RecoveryTerminal::Applied);
        true
    }

    fn lifecycle_cancel(&mut self) {
        let _ = self.fence.cancel_pending();
    }
}

#[test]
fn fake_api_covers_timeout_queue_duplicate_missing_retry_and_recovery() {
    let mut api = RecoveryApiHarness::default();
    api.document.replace("old preedit");

    assert_eq!(
        api.engine_timeout("old preedit", true, FakeRequestMode::Queued),
        RecoveryKeyDisposition::Consume
    );
    assert_eq!(api.queued.len(), 1, "one bounded finalizer is queued");
    assert_eq!(api.key_delete(), RecoveryKeyDisposition::Consume);
    assert_eq!(api.document.text, "old preedit", "delete cannot race ahead");

    // A retry while the first attempt owns the boundary is deduplicated.
    assert_eq!(
        api.engine_timeout("old preedit", true, FakeRequestMode::Queued),
        RecoveryKeyDisposition::Consume
    );
    assert_eq!(api.queued.len(), 1, "retry cannot duplicate the write");

    assert!(api.deliver(0));
    assert_eq!(
        api.document.writes, 2,
        "initial setup plus exactly one apply"
    );
    assert_eq!(api.key_delete(), RecoveryKeyDisposition::Host);
    assert_eq!(api.document.text, "");
    assert!(!api.deliver(0), "duplicate/missing callback is a no-op");

    // A missing callback remains fenced until a lifecycle terminal owns it.
    assert_eq!(
        api.engine_timeout("next", true, FakeRequestMode::Queued),
        RecoveryKeyDisposition::Consume
    );
    api.lifecycle_cancel();
    assert_eq!(api.key_delete(), RecoveryKeyDisposition::Host);
}

#[test]
fn fake_api_covers_boundaries_failure_reordering_external_change_and_sync_recovery() {
    let mut api = RecoveryApiHarness::default();
    assert_eq!(
        api.engine_timeout("", false, FakeRequestMode::Queued),
        RecoveryKeyDisposition::Host,
        "an empty idle document needs no finalizer"
    );
    assert_eq!(api.queued.len(), 0);

    assert_eq!(
        api.engine_timeout("rejected", true, FakeRequestMode::Rejected),
        RecoveryKeyDisposition::Host
    );
    assert!(!api.fence.is_pending());

    assert_eq!(
        api.engine_timeout("sync", true, FakeRequestMode::Synchronous),
        RecoveryKeyDisposition::Host
    );
    assert_eq!(api.document.text, "sync");

    assert_eq!(
        api.engine_timeout("stale", true, FakeRequestMode::Queued),
        RecoveryKeyDisposition::Consume
    );
    api.external_change_and_notify("host replacement");
    assert!(
        !api.deliver(0),
        "external lifecycle notification kills stale work"
    );
    assert_eq!(api.document.text, "host replacement");

    // Queue old, cancel it, queue new, then deliver callbacks in reverse age
    // order. The old token cannot edit or release the new generation.
    assert_eq!(
        api.engine_timeout("old", true, FakeRequestMode::Queued),
        RecoveryKeyDisposition::Consume
    );
    api.lifecycle_cancel();
    assert_eq!(
        api.engine_timeout("new", true, FakeRequestMode::Queued),
        RecoveryKeyDisposition::Consume
    );
    assert!(!api.deliver(0));
    assert!(api.fence.is_pending());
    assert!(api.deliver(0));
    assert_eq!(api.document.text, "new");

    // A cancelled callback still occupies the external scheduler's one-slot
    // queue. A new recovery must reject explicitly, not over-allocate or leave
    // an ownerless pending fence; after the stale callback drains, recovery is
    // available again.
    let mut bounded = RecoveryApiHarness::with_capacity(1);
    assert_eq!(
        bounded.engine_timeout("old", true, FakeRequestMode::Queued),
        RecoveryKeyDisposition::Consume
    );
    bounded.lifecycle_cancel();
    assert_eq!(
        bounded.engine_timeout("capacity", true, FakeRequestMode::Queued),
        RecoveryKeyDisposition::Host
    );
    assert!(!bounded.fence.is_pending());
    assert_eq!(bounded.queued.len(), 1);
    assert!(!bounded.deliver(0));
    assert_eq!(
        bounded.engine_timeout("recovered", true, FakeRequestMode::Queued),
        RecoveryKeyDisposition::Consume
    );
    assert!(bounded.deliver(0));
    assert_eq!(bounded.document.text, "recovered");
}

#[test]
fn recovery_domain_pbt_and_c2_cover_every_atomic_condition() {
    const SEED: u64 = 0x5341_4b55_5241_0057;
    let mut random = SEED;
    // Condition instances in the production module: begin has pending,
    // disposition token matches, finish token matches, cancel has pending.
    let mut condition_seen = [[false; 2]; 4];
    let mut compared = 0usize;

    for _ in 0..4_096 {
        random ^= random >> 12;
        random ^= random << 25;
        random ^= random >> 27;
        let bits = random.wrapping_mul(0x2545_f491_4f6c_dd1d);

        let mut fence = EngineRecoveryFence::default();
        let first = fence.begin().token();
        let mut oracle_pending = Some(first.id());
        condition_seen[0][0] = true;

        if bits & 1 != 0 {
            condition_seen[0][1] = true;
            assert!(matches!(fence.begin(), RecoveryStart::Deduplicated(_)));
        }

        condition_seen[3][1] = true;
        assert_eq!(
            fence.cancel_pending().is_some(),
            oracle_pending.take().is_some()
        );
        if bits & 2 != 0 {
            condition_seen[3][0] = true;
            assert_eq!(
                fence.cancel_pending().is_some(),
                oracle_pending.take().is_some()
            );
        }

        let current = fence.begin().token();
        oracle_pending = Some(current.id());
        let should_match = bits & 4 != 0;
        let callback = if should_match { current } else { first };
        condition_seen[1][usize::from(should_match)] = true;
        let expected = oracle_disposition(oracle_pending, callback.id());
        assert_eq!(
            fence.disposition_after_request(callback),
            match expected {
                OracleDisposition::Host => RecoveryKeyDisposition::Host,
                OracleDisposition::Consume => RecoveryKeyDisposition::Consume,
            }
        );

        condition_seen[2][usize::from(should_match)] = true;
        let expected_finished = oracle_finish(&mut oracle_pending, callback.id());
        let outcome = match (bits >> 3) % 3 {
            0 => RecoveryTerminal::Applied,
            1 => RecoveryTerminal::Rejected,
            _ => RecoveryTerminal::Cancelled,
        };
        assert_eq!(
            matches!(fence.finish(callback, outcome), RecoveryFinish::Finished(_)),
            expected_finished
        );
        assert_eq!(fence.is_pending(), oracle_pending.is_some());
        compared += 1;
    }

    // Deterministic tail closes any RNG-dependent C2 holes without changing
    // the PBT seed/replay evidence.
    let mut fence = EngineRecoveryFence::default();
    let stale = fence.begin().token();
    condition_seen[0][0] = true;
    condition_seen[0][1] = true;
    let _ = fence.begin();
    condition_seen[3][1] = true;
    let _ = fence.cancel_pending();
    condition_seen[3][0] = true;
    let _ = fence.cancel_pending();
    let current = fence.begin().token();
    condition_seen[1][0] = true;
    let _ = fence.disposition_after_request(stale);
    condition_seen[1][1] = true;
    let _ = fence.disposition_after_request(current);
    condition_seen[2][0] = true;
    let _ = fence.finish(stale, RecoveryTerminal::Applied);
    condition_seen[2][1] = true;
    let _ = fence.finish(current, RecoveryTerminal::Applied);

    let covered = condition_seen
        .into_iter()
        .flatten()
        .filter(|seen| *seen)
        .count();
    let total = 8usize;
    assert_eq!(covered, total);
    assert_eq!(compared, 4_096);
    eprintln!("recovery-domain PBT seed={SEED:#018x}; cases={compared}; C2={covered}/{total}=100%");
}
