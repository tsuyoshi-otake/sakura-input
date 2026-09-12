use crate::developer_history_oracle::{
    apply, apply_all, atomic_conditions, DomainEvent, OracleState, ScopeKind,
};

fn set_on() -> DomainEvent {
    DomainEvent::SetDeveloperMode { on: true }
}

fn set_off() -> DomainEvent {
    DomainEvent::SetDeveloperMode { on: false }
}

#[test]
fn observed_stale_inactive_class_is_forbidden_after_hot_enable() {
    // Reproduction of the live machine: boot with history off, flip the
    // setting on, publish, then a request (stats) must attach. The defective
    // production path left the service detached forever.
    let state = apply_all([
        DomainEvent::Boot {
            developer_mode: false,
        },
        set_on(),
        DomainEvent::WatcherPublish,
        DomainEvent::RequestBoundary,
    ]);
    assert!(state.setting_on);
    assert!(state.published_on);
    assert!(state.service_attached);
    assert!(state.stats_active());
    assert!(!state.forbidden_stale_inactive());
    assert!(state.attach_matches_published_after_request());
}

#[test]
fn publish_before_request_leaves_service_detached_until_boundary() {
    let mid = apply_all([
        DomainEvent::Boot {
            developer_mode: false,
        },
        set_on(),
        DomainEvent::WatcherPublish,
    ]);
    assert!(mid.published_on);
    assert!(!mid.service_attached);
    assert!(!mid.forbidden_stale_inactive());
    assert!(!mid.request_after_publish);

    let after = apply_all([
        DomainEvent::Boot {
            developer_mode: false,
        },
        set_on(),
        DomainEvent::WatcherPublish,
        DomainEvent::RequestBoundary,
    ]);
    assert!(after.service_attached);
}

#[test]
fn hot_disable_stops_new_durable_keys() {
    let state = apply_all([
        DomainEvent::Boot {
            developer_mode: true,
        },
        DomainEvent::Classify {
            scope: ScopeKind::Normal,
        },
        DomainEvent::Key { test_only: false },
        set_off(),
        DomainEvent::WatcherPublish,
        DomainEvent::RequestBoundary,
        DomainEvent::Key { test_only: false },
    ]);
    assert_eq!(state.durable.len(), 1);
    assert!(!state.service_attached);
    assert!(!state.stats_active());
}

#[test]
fn unclassified_password_and_test_only_are_excluded_not_durable() {
    let state = apply_all([
        DomainEvent::Boot {
            developer_mode: true,
        },
        DomainEvent::Classify {
            scope: ScopeKind::Unclassified,
        },
        DomainEvent::Key { test_only: false },
        DomainEvent::Classify {
            scope: ScopeKind::Sensitive,
        },
        DomainEvent::Key { test_only: false },
        DomainEvent::Classify {
            scope: ScopeKind::Normal,
        },
        DomainEvent::Key { test_only: true },
        DomainEvent::Key { test_only: false },
    ]);
    assert_eq!(state.durable.len(), 1);
    assert_eq!(state.excluded_unclassified, 1);
    assert_eq!(state.excluded_sensitive, 1);
    assert_eq!(state.excluded_test_only, 1);
}

#[test]
fn clear_empties_durable_and_bumps_epoch() {
    let state = apply_all([
        DomainEvent::Boot {
            developer_mode: true,
        },
        DomainEvent::Classify {
            scope: ScopeKind::Normal,
        },
        DomainEvent::Key { test_only: false },
        DomainEvent::Key { test_only: false },
        DomainEvent::Clear,
    ]);
    assert!(state.durable.is_empty());
    assert_eq!(state.epoch, 1);
}

#[test]
fn queue_full_drops_instead_of_recording() {
    let state = apply_all([
        DomainEvent::Boot {
            developer_mode: true,
        },
        DomainEvent::Classify {
            scope: ScopeKind::Normal,
        },
        DomainEvent::QueueFull,
        DomainEvent::Key { test_only: false },
    ]);
    assert!(state.durable.is_empty());
    assert_eq!(state.dropped, 1);
}

#[test]
fn crash_then_restart_restores_setting_and_keeps_durable() {
    let state = apply_all([
        DomainEvent::Boot {
            developer_mode: true,
        },
        DomainEvent::Classify {
            scope: ScopeKind::Normal,
        },
        DomainEvent::Key { test_only: false },
        DomainEvent::Crash,
        DomainEvent::Restart {
            restore_setting: true,
        },
    ]);
    assert!(state.live);
    assert!(state.service_attached);
    assert_eq!(state.durable.len(), 1);
}

#[test]
fn keys_while_detached_do_not_increment_exclusion_counters() {
    let state = apply_all([
        DomainEvent::Boot {
            developer_mode: false,
        },
        DomainEvent::Classify {
            scope: ScopeKind::Normal,
        },
        DomainEvent::Key { test_only: false },
        DomainEvent::Key { test_only: true },
    ]);
    assert!(state.durable.is_empty());
    assert_eq!(state.excluded_test_only, 0);
    assert_eq!(state.excluded_unclassified, 0);
}

#[test]
fn atomic_conditions_cover_both_polarities_in_a_short_campaign() {
    use std::collections::HashMap;

    let campaign = [
        DomainEvent::Boot {
            developer_mode: false,
        },
        set_on(),
        DomainEvent::WatcherPublish,
        DomainEvent::RequestBoundary,
        DomainEvent::Classify {
            scope: ScopeKind::Normal,
        },
        DomainEvent::Key { test_only: false },
        DomainEvent::Classify {
            scope: ScopeKind::Sensitive,
        },
        DomainEvent::Key { test_only: false },
        DomainEvent::Classify {
            scope: ScopeKind::Unclassified,
        },
        DomainEvent::Key { test_only: false },
        DomainEvent::Classify {
            scope: ScopeKind::Normal,
        },
        DomainEvent::Key { test_only: true },
        DomainEvent::QueueFull,
        DomainEvent::Key { test_only: false },
        set_off(),
        DomainEvent::WatcherPublish,
        DomainEvent::RequestBoundary,
    ];

    let mut seen: HashMap<&'static str, (bool, bool)> = HashMap::new();
    let mut state = OracleState::default();
    for event in campaign {
        for condition in atomic_conditions(&state, event) {
            let entry = seen.entry(condition.id).or_insert((false, false));
            if condition.value {
                entry.1 = true;
            } else {
                entry.0 = true;
            }
        }
        apply(&mut state, event);
    }

    for (id, (saw_false, saw_true)) in &seen {
        assert!(
            *saw_false && *saw_true,
            "{id} missing a polarity (false={saw_false}, true={saw_true})"
        );
    }
}

#[test]
fn shutdown_on_live_engine_does_not_count_as_stale_inactive() {
    let state = apply_all([
        DomainEvent::Boot {
            developer_mode: true,
        },
        DomainEvent::RequestBoundary,
        DomainEvent::Shutdown,
    ]);
    assert!(!state.live);
    assert!(!state.service_attached);
    assert!(!state.forbidden_stale_inactive());
    assert!(state.attach_matches_published_after_request());
}
