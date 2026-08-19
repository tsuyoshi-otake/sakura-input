//! COM-free Dual TSF arbitration for physical conversion keys.
//!
//! Cursor / Electron still constructs two `TextService` objects. This module
//! elects one thread-local IME. The other is a shadow: it never talks to the
//! engine or candidate UI, and it eats keys so Chromium cannot insert a
//! duplicate while the active instance owns the reading.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use sakura_proto::{KeyCode, KeyInput};

/// Sole IME vs host-constructed extra TIP on this thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImeSeat {
    Active,
    Shadow,
}

impl ImeSeat {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Shadow => "shadow",
        }
    }

    /// Only the elected IME may send keys or candidate updates to the engine.
    pub(crate) fn asks_engine(self) -> bool {
        matches!(self, Self::Active)
    }

    /// A shadow always consumes the physical key so Dual-delivery cannot
    /// insert into a second document.
    pub(crate) fn eats(self) -> bool {
        matches!(self, Self::Shadow)
    }
}

/// Thread-local sole-IME election. TSF key sinks in Cursor share one UI thread.
#[derive(Debug)]
pub(crate) struct ProcessSoleIme {
    primary: Mutex<Option<u64>>,
}

impl Default for ProcessSoleIme {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessSoleIme {
    pub(crate) const fn new() -> Self {
        Self {
            primary: Mutex::new(None),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Option<u64>> {
        match self.primary.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Focus gain steals the seat. Focus loss releases it only if we hold it.
    pub(crate) fn on_focus(&self, instance: u64, foreground: bool) {
        let mut primary = self.lock();
        if foreground {
            *primary = Some(instance);
        } else if *primary == Some(instance) {
            *primary = None;
        }
    }

    pub(crate) fn release(&self, instance: u64) {
        let mut primary = self.lock();
        if *primary == Some(instance) {
            *primary = None;
        }
    }

    /// Someone else already holds the seat and this instance has no reading.
    pub(crate) fn is_shadow(&self, instance: u64, local_live: bool) -> bool {
        if local_live {
            return false;
        }
        matches!(*self.lock(), Some(id) if id != instance)
    }

    /// First key on an empty seat claims it. A live reading always takes it.
    pub(crate) fn seat_for_key(&self, instance: u64, local_live: bool) -> ImeSeat {
        let mut primary = self.lock();
        if local_live {
            *primary = Some(instance);
            return ImeSeat::Active;
        }
        match *primary {
            Some(id) if id == instance => ImeSeat::Active,
            Some(_) => ImeSeat::Shadow,
            None => {
                *primary = Some(instance);
                ImeSeat::Active
            }
        }
    }
}

thread_local! {
    static THREAD_SOLE: ProcessSoleIme = const { ProcessSoleIme::new() };
}

pub(crate) fn with_sole<R>(f: impl FnOnce(&ProcessSoleIme) -> R) -> R {
    THREAD_SOLE.with(f)
}

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
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::HostEligible => "host_eligible",
            Self::ApplyLocal => "apply_local",
            Self::AbsorbPeer => "absorb_peer",
        }
    }

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

/// Shared candidate UI may End only from the instance that owns the popup.
///
/// `peer_live` covers same-process Dual TSF. Cursor / Electron also deliver
/// Space to a second process, where that table is empty; End authority is
/// then the local `ITfUIElement` lease. An idle peer with no lease must not
/// Hide a sibling's 履歴 / conversion list.
pub(crate) fn ends_shared_candidate_ui(local_live: bool, peer_live: bool, owns_ui: bool) -> bool {
    if peer_live && !local_live {
        false
    } else {
        owns_ui || local_live
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

    #[test]
    fn idle_peer_must_not_end_a_live_sibling_candidate_list() {
        assert!(
            ends_shared_candidate_ui(true, false, true),
            "the live owner may still Hide/End its own list"
        );
        assert!(
            ends_shared_candidate_ui(false, false, true),
            "the lease owner may End after the reading is gone"
        );
        assert!(
            !ends_shared_candidate_ui(false, false, false),
            "an idle Dual TSF without a lease must not End a sibling popup"
        );
        assert_eq!(ConversionKeyDisposition::AbsorbPeer.name(), "absorb_peer");
        assert!(
            !ends_shared_candidate_ui(false, true, false),
            "an idle Dual TSF peer must not Hide a sibling 履歴/conversion list"
        );
        assert!(
            ends_shared_candidate_ui(true, false, false),
            "a live reading may End even before the lease is adopted"
        );
    }

    #[test]
    fn first_key_elects_one_active_ime_and_the_other_is_shadow() {
        let sole = ProcessSoleIme::new();
        assert_eq!(sole.seat_for_key(1, false), ImeSeat::Active);
        assert_eq!(sole.seat_for_key(2, false), ImeSeat::Shadow);
        assert!(ImeSeat::Shadow.eats());
        assert!(!ImeSeat::Shadow.asks_engine());
        assert!(ImeSeat::Active.asks_engine());
        assert!(!ImeSeat::Active.eats());
    }

    #[test]
    fn focus_steals_the_sole_seat_from_an_idle_holder() {
        let sole = ProcessSoleIme::new();
        sole.on_focus(1, true);
        assert_eq!(sole.seat_for_key(1, false), ImeSeat::Active);
        sole.on_focus(2, true);
        assert_eq!(sole.seat_for_key(2, false), ImeSeat::Active);
        assert_eq!(sole.seat_for_key(1, false), ImeSeat::Shadow);
    }

    #[test]
    fn live_reading_takes_the_seat_even_if_focus_already_moved() {
        let sole = ProcessSoleIme::new();
        sole.on_focus(2, true);
        assert_eq!(sole.seat_for_key(1, true), ImeSeat::Active);
        assert_eq!(sole.seat_for_key(2, false), ImeSeat::Shadow);
    }

    #[test]
    fn drop_or_detach_releases_the_seat_for_the_next_instance() {
        let sole = ProcessSoleIme::new();
        assert_eq!(sole.seat_for_key(1, false), ImeSeat::Active);
        sole.release(1);
        assert_eq!(sole.seat_for_key(2, false), ImeSeat::Active);
    }

    #[test]
    fn shadow_without_a_peer_reading_is_still_not_an_engine_client() {
        assert!(!ImeSeat::Shadow.asks_engine());
        assert!(
            !ProcessSoleIme::new().is_shadow(1, true),
            "a live reading is never a shadow"
        );
    }
}
