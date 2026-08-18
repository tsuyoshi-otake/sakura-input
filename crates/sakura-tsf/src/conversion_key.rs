//! COM-free Dual TSF arbitration for physical conversion keys.
//!
//! A second idle `TextService` in the same process must not return Space to
//! the host while a sibling still owns the live reading.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use sakura_proto::{KeyCode, KeyInput};

/// Who may act on this physical conversion key from one `TextService`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConversionKeyDisposition {
    HostEligible,
    ApplyLocal,
    AbsorbPeer,
}

fn is_conversion_trigger(key: KeyInput) -> bool {
    !key.modifiers.ctrl()
        && !key.modifiers.alt()
        && matches!(key.code, KeyCode::Space | KeyCode::Henkan)
}

impl ConversionKeyDisposition {
    pub(crate) fn of(key: KeyInput, local_live: bool, peer_live: bool) -> Self {
        if !is_conversion_trigger(key) {
            return Self::HostEligible;
        }
        if local_live {
            Self::ApplyLocal
        } else if peer_live {
            Self::AbsorbPeer
        } else {
            Self::HostEligible
        }
    }

    pub(crate) fn eats(self, engine_consumed: bool) -> bool {
        match self {
            Self::HostEligible => engine_consumed,
            Self::ApplyLocal | Self::AbsorbPeer => true,
        }
    }

    pub(crate) fn asks_engine(self) -> bool {
        !matches!(self, Self::AbsorbPeer)
    }

    pub(crate) fn skip_probe_on_test_keydown(self) -> bool {
        matches!(self, Self::ApplyLocal | Self::AbsorbPeer)
    }
}

/// Numeric claim only. Never store COM objects here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClaimToken {
    instance: u64,
    generation: u64,
}

impl ClaimToken {
    pub(crate) fn new(instance: u64, generation: u64) -> Self {
        Self {
            instance,
            generation,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct LiveCompositionTable {
    live: Option<ClaimToken>,
}

impl LiveCompositionTable {
    pub(crate) fn publish(&mut self, token: ClaimToken) {
        match self.live {
            None => self.live = Some(token),
            Some(current) if current.instance == token.instance => self.live = Some(token),
            Some(_) => {}
        }
    }

    pub(crate) fn release(&mut self, token: ClaimToken) {
        if self.live == Some(token) {
            self.live = None;
        }
    }

    pub(crate) fn peer_live(&self, instance: u64) -> bool {
        self.live.is_some_and(|live| live.instance != instance)
    }
}

fn lock_table(table: &Mutex<LiveCompositionTable>) -> MutexGuard<'_, LiveCompositionTable> {
    match table.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Debug, Default)]
pub(crate) struct ProcessLiveCompositions {
    table: Mutex<LiveCompositionTable>,
}

impl ProcessLiveCompositions {
    pub(crate) fn sync(&self, token: ClaimToken, local_live: bool) {
        let mut table = lock_table(&self.table);
        if local_live {
            table.publish(token);
        } else {
            table.release(token);
        }
    }

    pub(crate) fn peer_live(&self, instance: u64) -> bool {
        lock_table(&self.table).peer_live(instance)
    }
}

pub(crate) fn allocate_instance() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn process_claims() -> &'static ProcessLiveCompositions {
    static CELL: OnceLock<ProcessLiveCompositions> = OnceLock::new();
    CELL.get_or_init(ProcessLiveCompositions::default)
}

#[cfg(test)]
mod tests {
    use sakura_proto::Modifiers;

    use super::*;

    fn space() -> KeyInput {
        KeyInput {
            code: KeyCode::Space,
            ch: None,
            modifiers: Modifiers::NONE,
            repeat: false,
            test_only: true,
        }
    }

    fn letter_a() -> KeyInput {
        KeyInput {
            code: KeyCode::Char,
            ch: Some('a'),
            modifiers: Modifiers::NONE,
            repeat: false,
            test_only: true,
        }
    }

    #[test]
    fn idle_space_is_host_eligible() {
        assert_eq!(
            ConversionKeyDisposition::of(space(), false, false),
            ConversionKeyDisposition::HostEligible
        );
    }

    #[test]
    fn local_live_space_is_apply_local() {
        assert_eq!(
            ConversionKeyDisposition::of(space(), true, false),
            ConversionKeyDisposition::ApplyLocal
        );
    }

    #[test]
    fn peer_live_space_is_absorb_peer() {
        assert_eq!(
            ConversionKeyDisposition::of(space(), false, true),
            ConversionKeyDisposition::AbsorbPeer
        );
    }

    #[test]
    fn peer_live_letter_stays_host_eligible() {
        assert_eq!(
            ConversionKeyDisposition::of(letter_a(), false, true),
            ConversionKeyDisposition::HostEligible
        );
    }

    #[test]
    fn ctrl_space_stays_host_eligible_during_live_reading() {
        let key = KeyInput {
            code: KeyCode::Space,
            ch: None,
            modifiers: Modifiers::CTRL,
            repeat: false,
            test_only: true,
        };
        assert_eq!(
            ConversionKeyDisposition::of(key, true, false),
            ConversionKeyDisposition::HostEligible
        );
    }

    #[test]
    fn absorb_peer_eats_even_when_engine_did_not_consume() {
        assert!(ConversionKeyDisposition::AbsorbPeer.eats(false));
        assert!(ConversionKeyDisposition::ApplyLocal.eats(false));
        assert!(!ConversionKeyDisposition::HostEligible.eats(false));
        assert!(ConversionKeyDisposition::HostEligible.eats(true));
    }

    #[test]
    fn absorb_peer_never_asks_engine() {
        assert!(!ConversionKeyDisposition::AbsorbPeer.asks_engine());
        assert!(ConversionKeyDisposition::ApplyLocal.asks_engine());
        assert!(ConversionKeyDisposition::HostEligible.asks_engine());
    }

    #[test]
    fn live_owner_and_peer_skip_probe_on_test_keydown() {
        assert!(ConversionKeyDisposition::ApplyLocal.skip_probe_on_test_keydown());
        assert!(ConversionKeyDisposition::AbsorbPeer.skip_probe_on_test_keydown());
        assert!(!ConversionKeyDisposition::HostEligible.skip_probe_on_test_keydown());
    }

    #[test]
    fn empty_table_has_no_peer() {
        let table = LiveCompositionTable::default();
        assert!(!table.peer_live(1));
    }

    #[test]
    fn published_claim_is_visible_to_a_sibling() {
        let mut table = LiveCompositionTable::default();
        table.publish(ClaimToken::new(1, 1));
        assert!(!table.peer_live(1));
        assert!(table.peer_live(2));
    }

    #[test]
    fn release_lets_idle_space_reach_the_host() {
        let mut table = LiveCompositionTable::default();
        let owner = ClaimToken::new(1, 1);
        table.publish(owner);
        table.release(owner);
        assert!(!table.peer_live(2));
        assert_eq!(
            ConversionKeyDisposition::of(space(), false, table.peer_live(2)),
            ConversionKeyDisposition::HostEligible
        );
    }

    #[test]
    fn stale_release_does_not_clear_a_newer_generation() {
        let mut table = LiveCompositionTable::default();
        let old = ClaimToken::new(1, 1);
        let new = ClaimToken::new(1, 2);
        table.publish(old);
        table.publish(new);
        table.release(old);
        assert!(table.peer_live(2));
    }

    #[test]
    fn foreign_publish_cannot_steal_a_live_claim() {
        let mut table = LiveCompositionTable::default();
        table.publish(ClaimToken::new(1, 1));
        table.publish(ClaimToken::new(2, 1));
        assert!(table.peer_live(2), "owner 1 must keep the claim");
        assert!(!table.peer_live(1));
    }

    #[test]
    fn process_table_teardown_returns_idle_space_to_host() {
        let claims = ProcessLiveCompositions::default();
        let owner = ClaimToken::new(10, 1);
        let peer = 11;
        claims.sync(owner, true);
        assert_eq!(
            ConversionKeyDisposition::of(space(), false, claims.peer_live(peer)),
            ConversionKeyDisposition::AbsorbPeer
        );
        claims.sync(owner, false);
        assert_eq!(
            ConversionKeyDisposition::of(space(), false, claims.peer_live(peer)),
            ConversionKeyDisposition::HostEligible
        );
    }

    #[test]
    fn two_state_owner_cannot_see_a_sibling_claim() {
        let space = space();
        assert_eq!(
            ConversionKeyDisposition::of(space, false, true),
            ConversionKeyDisposition::AbsorbPeer,
            "idle peer must absorb while a sibling owns the reading"
        );
        assert_eq!(
            ConversionKeyDisposition::of(space, false, false),
            ConversionKeyDisposition::HostEligible,
            "without a sibling claim the same Space is still host-owned"
        );
    }
}
