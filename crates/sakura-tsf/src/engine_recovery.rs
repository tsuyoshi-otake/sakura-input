//! COM-free ownership fence for an engine-timeout composition finalizer.
//!
//! A timed-out key is transport-ambiguous: the engine may have consumed it,
//! but Sakura Input has not applied an answer to the host document.  When the
//! old visible composition can only be finalized through an asynchronous edit
//! session, returning that key (or a later key) to the host lets the host edit
//! ahead of the old finalizer.  This one-slot token fence keeps those keys
//! consumed until the exact finalizer reaches a terminal outcome.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct RecoveryToken(u64);

impl RecoveryToken {
    #[cfg(test)]
    pub(crate) fn id(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryStart {
    Started(RecoveryToken),
    Deduplicated(RecoveryToken),
}

impl RecoveryStart {
    pub(crate) fn token(self) -> RecoveryToken {
        match self {
            Self::Started(token) | Self::Deduplicated(token) => token,
        }
    }

    pub(crate) fn is_deduplicated(self) -> bool {
        matches!(self, Self::Deduplicated(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryTerminal {
    Applied,
    Rejected,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecoveryTerminalRecord {
    pub(crate) token: RecoveryToken,
    pub(crate) outcome: RecoveryTerminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryFinish {
    Finished(RecoveryTerminalRecord),
    IgnoredStale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryKeyDisposition {
    Host,
    Consume,
}

/// At most one old-composition finalizer may fence keys on a TSF thread.
///
/// The token, rather than a bare boolean, is essential: a delayed completion
/// from an older lifecycle must not clear the authority of a newer recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct EngineRecoveryFence {
    next_token: u64,
    pending: Option<RecoveryToken>,
}

impl EngineRecoveryFence {
    pub(crate) fn begin(&mut self) -> RecoveryStart {
        if let Some(token) = self.pending {
            return RecoveryStart::Deduplicated(token);
        }

        let raw = self.next_token.max(1);
        let token = RecoveryToken(raw);
        self.next_token = raw.wrapping_add(1).max(1);
        self.pending = Some(token);
        RecoveryStart::Started(token)
    }

    pub(crate) fn is_pending(self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn owns(self, token: RecoveryToken) -> bool {
        self.pending == Some(token)
    }

    pub(crate) fn disposition_after_request(self, token: RecoveryToken) -> RecoveryKeyDisposition {
        if self.owns(token) {
            RecoveryKeyDisposition::Consume
        } else {
            RecoveryKeyDisposition::Host
        }
    }

    pub(crate) fn finish(
        &mut self,
        token: RecoveryToken,
        outcome: RecoveryTerminal,
    ) -> RecoveryFinish {
        if !self.owns(token) {
            return RecoveryFinish::IgnoredStale;
        }
        self.pending = None;
        RecoveryFinish::Finished(RecoveryTerminalRecord { token, outcome })
    }

    pub(crate) fn cancel_pending(&mut self) -> Option<RecoveryTerminalRecord> {
        let token = self.pending.take()?;
        Some(RecoveryTerminalRecord {
            token,
            outcome: RecoveryTerminal::Cancelled,
        })
    }
}
