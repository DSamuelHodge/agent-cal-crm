//! Per-channel kill-switch (ROADMAP Phase 2.5).
//!
//! A [`ChannelGate`] holds the set of disabled channels. The hot check
//! [`ChannelGate::is_enabled`] is a single expression — one `HashSet` lookup,
//! no loops, no I/O — so the per-request cost is provably one atomic load.
//! Enforcement happens at two levels:
//!
//! - the dispatch wrapper ([`crate::rpc::dispatch_with_gate`]) rejects at the
//!   TOP of the dispatch path, before send-gating/approval logic, for every
//!   method carrying a channel;
//! - the underlying send path ([`crate::inbox::ingest_with_gate`]) rejects
//!   too, so calling the agent function directly — bypassing dispatch — still
//!   fails closed.
//!
//! Default: every known channel is enabled. Known channels mirror
//! [`crate::inbox::kind_for_channel`]: `sms`, `email`, `whatsapp`, `call`,
//! `push`.
//!
//! TODO(config): plumb enable/disable from daemon config (env/file) and,
//! if needed, an admin RPC toggle. For now the gate is constructed in code
//! (`ChannelGate::new` + [`ChannelGate::disable`] / [`ChannelGate::enable`])
//! with `TODO` as the only wiring — no config-file or RPC plumbing in scope.

use std::collections::HashSet;

use crate::error::{AgentError, Result};

/// Channels enabled by default. Mirrors `src/inbox.rs::kind_for_channel`
/// (canonical names only; aliases collapse via [`normalize_channel`]).
pub const KNOWN_CHANNELS: &[&str] = &["sms", "email", "whatsapp", "call", "push"];

/// Collapse aliases to the canonical five in [`KNOWN_CHANNELS`]:
/// `wa` → `whatsapp`, `mail` → `email`, `phone`/`voice` → `call`,
/// `notification` → `push`. Unknown names pass through lowercased/trimmed.
pub fn normalize_channel(channel: &str) -> String {
    match channel.trim().to_lowercase().as_str() {
        "wa" => "whatsapp".to_string(),
        "mail" => "email".to_string(),
        "phone" | "voice" => "call".to_string(),
        "notification" => "push".to_string(),
        other => other.to_string(),
    }
}

/// Per-channel kill-switch: the set of disabled (killed) channels.
///
/// Cheap to clone; share by reference (`&ChannelGate`) per request.
#[derive(Debug, Clone, Default)]
pub struct ChannelGate {
    disabled: HashSet<String>,
}

impl ChannelGate {
    /// All channels enabled (empty disabled set).
    pub fn new() -> Self {
        Self {
            disabled: HashSet::new(),
        }
    }

    /// Constructor for tests: start with `channels` disabled.
    pub fn with_disabled(channels: &[&str]) -> Self {
        let mut gate = Self::new();
        for c in channels {
            gate.disable(c);
        }
        gate
    }

    /// Kill `channel` (idempotent).
    pub fn disable(&mut self, channel: &str) {
        self.disabled.insert(normalize_channel(channel));
    }

    /// Re-enable `channel` (idempotent).
    pub fn enable(&mut self, channel: &str) {
        self.disabled.remove(normalize_channel(channel).as_str());
    }

    /// PROVABLE SINGLE STATEMENT: one `HashSet` lookup — no loops, no I/O.
    /// Keep this body to exactly one expression so it holds by inspection.
    pub fn is_enabled(&self, channel: &str) -> bool {
        !self.disabled.contains(&normalize_channel(channel))
    }

    /// Fail closed with [`AgentError::ChannelDisabled`] when `channel` is
    /// killed; `Ok(())` otherwise.
    pub fn ensure_enabled(&self, channel: &str) -> Result<()> {
        if self.is_enabled(channel) {
            Ok(())
        } else {
            Err(AgentError::ChannelDisabled(normalize_channel(channel)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_by_default() {
        let gate = ChannelGate::new();
        for c in KNOWN_CHANNELS {
            assert!(gate.is_enabled(c), "{c}");
        }
    }
}
