//! Independent domain oracle for Space during Japanese conversion.
//!
//! This module answers a user-facing question only: given one physical Space
//! key delivered to a set of engine connections, may the document gain a
//! space, may a composition convert, or both? It does not import the engine
//! dispatcher, session table, key map, TSF, or named-pipe server.
//!
//! Requirements encoded here (see `verification/space-key-dispatch/requirements.md`):
//! - Space on a composing/converting connection converts and never inserts.
//! - Space on an idle Japanese-mode connection inserts one fullwidth space
//!   only when no peer connection is composing or converting.
//! - One physical Space must not both insert a document space and convert.
//! - Crash/restart of a connection drops its composition and does not replay
//!   a later convert for that same Space.
//! - A crash/restart that dropped a *live* composition absorbs exactly one
//!   following idle Space, so the reading the user was still editing does
//!   not become a document space on the connection that replaced it (#102).
//! - Resource exhaustion refuses additional document spaces.

/// Maximum modeled connections (actors).
pub const MAX_CONNECTIONS: usize = 3;
/// Maximum fullwidth spaces the document may accept.
pub const MAX_DOCUMENT_SPACES: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Idle,
    Composing,
    Converting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainEvent {
    /// An accepted start/continuation of a reading, not every raw Type key.
    /// The production live-peer policy may suppress a raw idle-peer key.
    Type {
        connection: u8,
    },
    /// One physical Space delivered to the bit-mask of connections.
    Space {
        targets: u8,
    },
    Commit {
        connection: u8,
    },
    Cancel {
        connection: u8,
    },
    /// Abandon a context with its reading still live, then reconnect.
    /// Orderly replacement after commit/cancel is not a teardown credit.
    AbandonContext {
        connection: u8,
    },
    /// Drop/reopen a connection while the engine's shared fence survives.
    /// This does not model killing and recreating the entire engine process.
    CrashRestart {
        connection: u8,
    },
    /// The connection is gone and must ignore further keys until restart.
    Disconnect {
        connection: u8,
    },
    /// Space delivered while the worker is busy: no document change.
    TimeoutSpace {
        targets: u8,
    },
    /// The host omitted the Space entirely.
    DropSpace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Connection {
    pub state: ConnState,
    pub live: bool,
}

impl Default for Connection {
    fn default() -> Self {
        Self {
            state: ConnState::Idle,
            live: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleState {
    pub connections: [Connection; MAX_CONNECTIONS],
    pub actors: usize,
    pub document_spaces: u32,
    pub conversions: u32,
    pub absorbed: u32,
    pub timeouts: u32,
    pub crashes: u32,
    /// A composition was torn down while live and owes the next idle Space
    /// an absorption. One-shot and untimed: the model has no wall clock, and
    /// neither does the implementation it mirrors.
    pub pending_teardown: bool,
    pub last_inserted: bool,
    pub last_converted: bool,
    pub last_absorbed: bool,
    pub logical_time: u32,
}

impl OracleState {
    pub fn new(actors: usize) -> Self {
        let actors = actors.clamp(1, MAX_CONNECTIONS);
        Self {
            connections: [Connection::default(); MAX_CONNECTIONS],
            actors,
            document_spaces: 0,
            conversions: 0,
            absorbed: 0,
            timeouts: 0,
            crashes: 0,
            pending_teardown: false,
            last_inserted: false,
            last_converted: false,
            last_absorbed: false,
            logical_time: 0,
        }
    }
}

impl Default for OracleState {
    fn default() -> Self {
        Self::new(2)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceEffect {
    Convert,
    Insert,
    Absorb,
    Ignore,
}

pub fn peer_converting(state: &OracleState, connection: usize) -> bool {
    (0..state.actors).any(|other| {
        other != connection
            && state.connections[other].live
            && matches!(
                state.connections[other].state,
                ConnState::Composing | ConnState::Converting
            )
    })
}

pub fn space_effect(state: &OracleState, connection: usize) -> SpaceEffect {
    if connection >= state.actors || !state.connections[connection].live {
        return SpaceEffect::Ignore;
    }
    match state.connections[connection].state {
        ConnState::Composing | ConnState::Converting => SpaceEffect::Convert,
        ConnState::Idle if peer_converting(state, connection) => SpaceEffect::Absorb,
        ConnState::Idle if state.pending_teardown => SpaceEffect::Absorb,
        ConnState::Idle if state.document_spaces >= MAX_DOCUMENT_SPACES => SpaceEffect::Absorb,
        ConnState::Idle => SpaceEffect::Insert,
    }
}

/// `true` when this connection still held a reading the user was editing.
/// Committing or cancelling reaches `ConnState::Idle` first, so an orderly
/// ending is already indistinguishable from never having composed.
fn was_live_reading(state: &OracleState, index: usize) -> bool {
    state.connections[index].live
        && matches!(
            state.connections[index].state,
            ConnState::Composing | ConnState::Converting
        )
}

fn valid_connection(state: &OracleState, connection: u8) -> Option<usize> {
    let index = usize::from(connection);
    (index < state.actors).then_some(index)
}

fn clear_last_key(state: &mut OracleState) {
    state.last_inserted = false;
    state.last_converted = false;
    state.last_absorbed = false;
}

pub fn apply(state: &mut OracleState, event: DomainEvent) {
    state.logical_time = state.logical_time.saturating_add(1);
    match event {
        DomainEvent::Type { connection } => {
            clear_last_key(state);
            if let Some(index) = valid_connection(state, connection) {
                if state.connections[index].live {
                    state.connections[index].state = ConnState::Composing;
                    // Composing again proves the input path recovered, so the
                    // teardown has nothing left to protect. Without this a
                    // flapping connection would bank absorptions and swallow
                    // a Space the user typed much later.
                    state.pending_teardown = false;
                }
            }
        }
        DomainEvent::Space { targets } => apply_space(state, targets),
        DomainEvent::Commit { connection } | DomainEvent::Cancel { connection } => {
            clear_last_key(state);
            if let Some(index) = valid_connection(state, connection) {
                if state.connections[index].live {
                    state.connections[index].state = ConnState::Idle;
                }
            }
        }
        DomainEvent::AbandonContext { connection } => {
            clear_last_key(state);
            if let Some(index) = valid_connection(state, connection) {
                if state.connections[index].live {
                    if was_live_reading(state, index) {
                        state.pending_teardown = true;
                    }
                    state.connections[index].state = ConnState::Idle;
                }
            }
        }
        DomainEvent::CrashRestart { connection } => {
            clear_last_key(state);
            if let Some(index) = valid_connection(state, connection) {
                // Read the outgoing state first: the restart overwrites it,
                // and whether a reading was live is the whole reason a
                // teardown owes the next Space an absorption.
                if was_live_reading(state, index) {
                    state.pending_teardown = true;
                }
                state.connections[index] = Connection::default();
                state.crashes = state.crashes.saturating_add(1);
            }
        }
        DomainEvent::Disconnect { connection } => {
            clear_last_key(state);
            if let Some(index) = valid_connection(state, connection) {
                // The dead target receives no more keys, but a surviving
                // idle peer of the same host can encounter the lost reading.
                if was_live_reading(state, index) {
                    state.pending_teardown = true;
                }
                state.connections[index].live = false;
                state.connections[index].state = ConnState::Idle;
            }
        }
        DomainEvent::TimeoutSpace { targets } => {
            clear_last_key(state);
            if targets != 0 {
                state.timeouts = state.timeouts.saturating_add(1);
                state.last_absorbed = true;
                state.absorbed = state.absorbed.saturating_add(1);
            }
        }
        DomainEvent::DropSpace => {
            clear_last_key(state);
        }
    }
}

fn was_idle_target(state: &OracleState, index: usize) -> bool {
    state.connections[index].live && state.connections[index].state == ConnState::Idle
}

fn apply_space(state: &mut OracleState, targets: u8) {
    clear_last_key(state);
    if targets == 0 {
        return;
    }
    let mut inserted = false;
    let mut converted = false;
    let mut absorbed = false;
    let mut spent_teardown = false;
    let mut next = state.connections;
    for (index, connection) in next.iter_mut().enumerate().take(state.actors) {
        if (targets & (1 << index)) == 0 {
            continue;
        }
        // A live peer already owns the absorption; keep the lost reading's
        // credit until no live reading can handle the user's Space.
        if state.pending_teardown && was_idle_target(state, index) && !peer_converting(state, index)
        {
            spent_teardown = true;
        }
        match space_effect(state, index) {
            SpaceEffect::Convert => {
                connection.state = ConnState::Converting;
                converted = true;
            }
            SpaceEffect::Insert => {
                inserted = true;
            }
            SpaceEffect::Absorb => {
                absorbed = true;
            }
            SpaceEffect::Ignore => {}
        }
    }
    // Safety: one physical Space cannot both insert and convert. If the
    // environment targeted both an idle and a composing connection, idle
    // insertion is absorbed.
    if inserted && converted {
        inserted = false;
        absorbed = true;
    }
    if spent_teardown {
        state.pending_teardown = false;
    }
    state.connections = next;
    state.last_inserted = inserted;
    state.last_converted = converted;
    state.last_absorbed = absorbed;
    if inserted {
        state.document_spaces = state.document_spaces.saturating_add(1);
    }
    if converted {
        state.conversions = state.conversions.saturating_add(1);
    }
    if absorbed {
        state.absorbed = state.absorbed.saturating_add(1);
    }
}

pub fn apply_all<I>(actors: usize, events: I) -> OracleState
where
    I: IntoIterator<Item = DomainEvent>,
{
    let mut state = OracleState::new(actors);
    for event in events {
        apply(&mut state, event);
    }
    state
}

pub fn no_dual_effect(state: &OracleState) -> bool {
    !(state.last_inserted && state.last_converted)
}

/// Atomic conditions for C2. Each pair is `(id, observed_true)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtomicCondition {
    pub id: &'static str,
    pub value: bool,
}

pub const ATOM_COUNT: usize = 11;

pub fn atomic_conditions(state: &OracleState, event: DomainEvent) -> [AtomicCondition; ATOM_COUNT] {
    let connection = match event {
        DomainEvent::Type { connection }
        | DomainEvent::Commit { connection }
        | DomainEvent::Cancel { connection }
        | DomainEvent::AbandonContext { connection }
        | DomainEvent::CrashRestart { connection }
        | DomainEvent::Disconnect { connection } => usize::from(connection),
        DomainEvent::Space { targets } | DomainEvent::TimeoutSpace { targets } => (0..state.actors)
            .find(|index| (targets & (1 << index)) != 0)
            .unwrap_or(0),
        DomainEvent::DropSpace => 0,
    };
    let conn = state
        .connections
        .get(connection)
        .copied()
        .unwrap_or_default();
    let dual = matches!(event, DomainEvent::Space { targets } if targets.count_ones() > 1);
    [
        AtomicCondition {
            id: "ATOM-IDLE",
            value: conn.state == ConnState::Idle,
        },
        AtomicCondition {
            id: "ATOM-COMPOSING",
            value: conn.state == ConnState::Composing,
        },
        AtomicCondition {
            id: "ATOM-CONVERTING",
            value: conn.state == ConnState::Converting,
        },
        AtomicCondition {
            id: "ATOM-LIVE",
            value: conn.live,
        },
        AtomicCondition {
            id: "ATOM-PEER-CONVERTING",
            value: peer_converting(state, connection),
        },
        AtomicCondition {
            id: "ATOM-SPACE-EVENT",
            value: matches!(event, DomainEvent::Space { .. }),
        },
        AtomicCondition {
            id: "ATOM-DUAL-TARGET",
            value: dual,
        },
        AtomicCondition {
            id: "ATOM-SPACE-EXHAUSTED",
            value: state.document_spaces >= MAX_DOCUMENT_SPACES,
        },
        AtomicCondition {
            id: "ATOM-TYPE-EVENT",
            value: matches!(event, DomainEvent::Type { .. }),
        },
        AtomicCondition {
            id: "ATOM-CRASH-EVENT",
            value: matches!(event, DomainEvent::CrashRestart { .. }),
        },
        AtomicCondition {
            id: "ATOM-PENDING-TEARDOWN",
            value: state.pending_teardown,
        },
    ]
}
