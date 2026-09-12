//! Independent domain oracle for developer-input-history lifecycle.
//!
//! This module answers a user-facing question only: given a sequence of
//! lifecycle events, is the history service attached, and which keys are
//! allowed to become durable? It does not import the engine dispatcher,
//! session table, named-pipe server, or `InputHistoryService` writer.
//! Production code may be compared against it; the oracle must not be
//! derived by copying production control flow.
//!
//! Requirements this encodes:
//! - `developer-mode` ON must attach the history service after the
//!   configuration is published and a request boundary runs, without an
//!   engine process restart.
//! - `developer-mode` OFF must detach at the next request boundary.
//! - Only Normal, positively classified, non-`test_only` keys may become
//!   durable while the service is attached.
//! - `stats.active` equals `service_attached`.
//! - The observed stale-inactive class is forbidden: setting published ON,
//!   at least one request after that publish, and the service still
//!   detached.

/// User-facing / lifecycle events. Characters and key codes are omitted:
/// this oracle tracks attach policy and durable eligibility, not the
/// contents of a keystroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainEvent {
    Boot { developer_mode: bool },
    SetDeveloperMode { on: bool },
    WatcherPublish,
    RequestBoundary,
    Classify { scope: ScopeKind },
    Key { test_only: bool },
    Flush,
    Clear,
    QueueFull,
    PersistFail,
    Crash,
    Restart { restore_setting: bool },
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Unclassified,
    Normal,
    Sensitive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableKey {
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleState {
    pub setting_on: bool,
    pub published_on: bool,
    pub service_attached: bool,
    pub live: bool,
    pub scope: ScopeKind,
    pub durable: Vec<DurableKey>,
    pub next_sequence: u64,
    pub dropped: u64,
    pub excluded_unclassified: u64,
    pub excluded_sensitive: u64,
    pub excluded_test_only: u64,
    pub persistence_failures: u64,
    pub epoch: u64,
    pub request_after_publish: bool,
    pub pending_publish: bool,
    pub queue_full: bool,
}

impl Default for OracleState {
    fn default() -> Self {
        Self {
            setting_on: false,
            published_on: false,
            service_attached: false,
            live: false,
            scope: ScopeKind::Unclassified,
            durable: Vec::new(),
            next_sequence: 0,
            dropped: 0,
            excluded_unclassified: 0,
            excluded_sensitive: 0,
            excluded_test_only: 0,
            persistence_failures: 0,
            epoch: 0,
            request_after_publish: false,
            pending_publish: false,
            queue_full: false,
        }
    }
}

impl OracleState {
    pub fn stats_active(&self) -> bool {
        self.service_attached
    }

    /// The observed production defect class: published ON, a later request,
    /// and the service still detached on a live engine.
    pub fn forbidden_stale_inactive(&self) -> bool {
        self.live && self.published_on && self.request_after_publish && !self.service_attached
    }

    pub fn attach_matches_published_after_request(&self) -> bool {
        !self.live || !self.request_after_publish || self.service_attached == self.published_on
    }
}

fn sync_attach(state: &mut OracleState) {
    state.service_attached = state.published_on;
    state.request_after_publish = true;
}

pub fn apply(state: &mut OracleState, event: DomainEvent) {
    match event {
        DomainEvent::Boot { developer_mode } => {
            *state = OracleState {
                setting_on: developer_mode,
                published_on: developer_mode,
                service_attached: developer_mode,
                live: true,
                scope: ScopeKind::Unclassified,
                durable: state.durable.clone(),
                next_sequence: state.next_sequence,
                epoch: state.epoch,
                ..OracleState::default()
            };
            state.live = true;
            state.setting_on = developer_mode;
            state.published_on = developer_mode;
            state.service_attached = developer_mode;
        }
        DomainEvent::SetDeveloperMode { on } => {
            state.setting_on = on;
            state.pending_publish = true;
        }
        DomainEvent::WatcherPublish => {
            if state.pending_publish || state.setting_on != state.published_on {
                state.published_on = state.setting_on;
                state.pending_publish = false;
                state.request_after_publish = false;
            }
        }
        DomainEvent::RequestBoundary => {
            if state.live {
                sync_attach(state);
            }
        }
        DomainEvent::Classify { scope } => {
            state.scope = scope;
        }
        DomainEvent::Key { test_only } => {
            if !state.service_attached {
                return;
            }
            if test_only {
                state.excluded_test_only = state.excluded_test_only.saturating_add(1);
                return;
            }
            match state.scope {
                ScopeKind::Unclassified => {
                    state.excluded_unclassified = state.excluded_unclassified.saturating_add(1);
                }
                ScopeKind::Sensitive => {
                    state.excluded_sensitive = state.excluded_sensitive.saturating_add(1);
                }
                ScopeKind::Normal => {
                    if state.queue_full {
                        state.dropped = state.dropped.saturating_add(1);
                        return;
                    }
                    state.next_sequence = state.next_sequence.saturating_add(1);
                    state.durable.push(DurableKey {
                        sequence: state.next_sequence,
                    });
                }
            }
        }
        DomainEvent::Flush => {
            // Flush is a no-op on the oracle's durable vector: records are
            // already considered durable once accepted.
        }
        DomainEvent::Clear => {
            state.epoch = state.epoch.saturating_add(1);
            state.durable.clear();
            state.queue_full = false;
        }
        DomainEvent::QueueFull => {
            state.queue_full = true;
        }
        DomainEvent::PersistFail => {
            state.persistence_failures = state.persistence_failures.saturating_add(1);
        }
        DomainEvent::Crash => {
            state.live = false;
            state.service_attached = false;
            state.request_after_publish = false;
            state.pending_publish = false;
            state.queue_full = false;
        }
        DomainEvent::Restart { restore_setting } => {
            let durable = state.durable.clone();
            let next_sequence = state.next_sequence;
            let epoch = state.epoch;
            let setting = if restore_setting {
                state.setting_on
            } else {
                false
            };
            *state = OracleState {
                setting_on: setting,
                published_on: setting,
                service_attached: setting,
                live: true,
                durable,
                next_sequence,
                epoch,
                ..OracleState::default()
            };
            state.live = true;
            state.setting_on = setting;
            state.published_on = setting;
            state.service_attached = setting;
        }
        DomainEvent::Shutdown => {
            state.live = false;
            state.service_attached = false;
            state.request_after_publish = false;
            state.pending_publish = false;
        }
    }
}

pub fn apply_all<I>(events: I) -> OracleState
where
    I: IntoIterator<Item = DomainEvent>,
{
    let mut state = OracleState::default();
    for event in events {
        apply(&mut state, event);
    }
    state
}

/// Atomic conditions used for C2 measurement of this oracle. Each pair is
/// `(id, observed_true)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtomicCondition {
    pub id: &'static str,
    pub value: bool,
}

pub fn atomic_conditions(state: &OracleState, event: DomainEvent) -> [AtomicCondition; 10] {
    let key_test_only = matches!(event, DomainEvent::Key { test_only: true });
    let key_real = matches!(event, DomainEvent::Key { test_only: false });
    [
        AtomicCondition {
            id: "setting_on",
            value: state.setting_on,
        },
        AtomicCondition {
            id: "published_on",
            value: state.published_on,
        },
        AtomicCondition {
            id: "service_attached",
            value: state.service_attached,
        },
        AtomicCondition {
            id: "request_after_publish",
            value: state.request_after_publish,
        },
        AtomicCondition {
            id: "scope_normal",
            value: matches!(state.scope, ScopeKind::Normal),
        },
        AtomicCondition {
            id: "scope_sensitive",
            value: matches!(state.scope, ScopeKind::Sensitive),
        },
        AtomicCondition {
            id: "scope_unclassified",
            value: matches!(state.scope, ScopeKind::Unclassified),
        },
        AtomicCondition {
            id: "event_key_test_only",
            value: key_test_only,
        },
        AtomicCondition {
            id: "event_key_real",
            value: key_real,
        },
        AtomicCondition {
            id: "queue_full",
            value: state.queue_full,
        },
    ]
}
