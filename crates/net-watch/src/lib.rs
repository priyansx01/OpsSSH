//! Deterministic reconnect policy. Platform network events may trigger `network_available`.
use serde::Deserialize;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Connected,
    Waiting { attempt: u32, due: Instant },
    Connecting { attempt: u32 },
    Blocked,
    Stopped,
}
#[derive(Debug, Clone)]
pub struct Reconnect {
    state: State,
    generation: u64,
    identity: String,
}
impl Reconnect {
    pub fn new(verified_host_identity: String) -> Self {
        Self {
            state: State::Connected,
            generation: 0,
            identity: verified_host_identity,
        }
    }
    pub fn state(&self) -> &State {
        &self.state
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn accepts_input(&self, generation: u64) -> bool {
        self.state == State::Connected && self.generation == generation
    }
    pub fn disconnected(&mut self, now: Instant) {
        if self.state == State::Connected {
            self.generation = self.generation.wrapping_add(1);
            self.state = State::Waiting {
                attempt: 0,
                due: now,
            };
        }
    }
    pub fn begin_if_due(&mut self, now: Instant) -> bool {
        if let State::Waiting { attempt, due } = self.state
            && now >= due
        {
            self.state = State::Connecting { attempt };
            return true;
        }
        false
    }
    pub fn failed(&mut self, now: Instant, retryable: bool) {
        if let State::Connecting { attempt } = self.state {
            self.state = if retryable {
                State::Waiting {
                    attempt: attempt.saturating_add(1),
                    due: now + Duration::from_secs((1_u64 << attempt.min(5)).min(30)),
                }
            } else {
                State::Blocked
            };
        }
    }
    /// Host-key or authentication failures block. Never lower verification on reconnect.
    pub fn connected(&mut self, verified_host_identity: &str) -> bool {
        if !matches!(self.state, State::Connecting { .. }) {
            return false;
        }
        if self.identity != verified_host_identity {
            self.state = State::Blocked;
            false
        } else {
            self.state = State::Connected;
            true
        }
    }
    pub fn network_available(&mut self, now: Instant) {
        if let State::Waiting { attempt, .. } = self.state {
            self.state = State::Waiting { attempt, due: now };
        }
    }
    pub fn stop(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.state = State::Stopped;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VpnStatus {
    Connected,
    Reconnecting,
    Down,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VpnMessage {
    state: VpnStatus,
}
/// Input is one bounded line from a locally authenticated IPC peer. This parser
/// does not establish peer identity; socket permissions belong to platform.
pub fn parse_vpn_line(line: &[u8]) -> Result<VpnStatus, &'static str> {
    if line.len() > 1024 || line.is_empty() {
        return Err("VPN status line must contain 1..1024 bytes");
    }
    serde_json::from_slice::<VpnMessage>(line)
        .map(|message| message.state)
        .map_err(|_| "invalid VPN status message")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_input_is_never_replayed() {
        let now = Instant::now();
        let mut r = Reconnect::new("SHA256:a".into());
        let old = r.generation();
        r.disconnected(now);
        assert!(!r.accepts_input(old));
        assert!(r.begin_if_due(now));
        assert!(r.connected("SHA256:a"));
        assert!(!r.accepts_input(old));
        assert!(r.accepts_input(r.generation()));
    }
    #[test]
    fn identity_change_blocks_and_network_does_not_override() {
        let now = Instant::now();
        let mut r = Reconnect::new("a".into());
        r.disconnected(now);
        r.begin_if_due(now);
        assert!(!r.connected("b"));
        r.network_available(now);
        assert_eq!(r.state(), &State::Blocked);
    }
    #[test]
    fn backoff_caps_and_network_returns_immediately() {
        let mut now = Instant::now();
        let mut r = Reconnect::new("a".into());
        r.disconnected(now);
        for _ in 0..40 {
            assert!(r.begin_if_due(now));
            r.failed(now, true);
            if let State::Waiting { due, .. } = *r.state() {
                assert!(due.duration_since(now) <= Duration::from_secs(30));
                now = due;
            }
        }
        r.network_available(now);
        assert!(r.begin_if_due(now));
    }
    #[test]
    fn vpn_protocol_rejects_extra_fields_and_oversized_input() {
        assert_eq!(
            parse_vpn_line(br#"{"state":"connected"}"#).unwrap(),
            VpnStatus::Connected
        );
        assert!(parse_vpn_line(br#"{"state":"up"}"#).is_err());
        assert!(parse_vpn_line(br#"{"state":"connected","command":"run"}"#).is_err());
        assert!(parse_vpn_line(&[b' '; 1025]).is_err());
    }
}
