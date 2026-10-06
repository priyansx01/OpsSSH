//! Local PTY workers, damage-based scheduling, and the optional GPUI terminal view.

mod session;
pub use session::{LocalSession, SessionNotice};

#[cfg(feature = "gui")]
mod gpu;
#[cfg(feature = "gui")]
pub use gpu::TerminalView;

use std::collections::BTreeSet;
use std::time::Duration;

use opsssh_term_core::{SessionState, TerminalSnapshot};

pub const SYNC_TIMEOUT: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameDamage {
    pub full: bool,
    pub rows: BTreeSet<usize>,
    pub cursor: bool,
}

impl FrameDamage {
    fn is_empty(&self) -> bool {
        !self.full && self.rows.is_empty() && !self.cursor
    }

    fn merge(&mut self, other: Self) {
        self.full |= other.full;
        self.cursor |= other.cursor;
        if self.full {
            self.rows.clear();
        } else {
            self.rows.extend(other.rows);
        }
    }
}

impl From<&TerminalSnapshot> for FrameDamage {
    fn from(snapshot: &TerminalSnapshot) -> Self {
        let full = snapshot.full_damage;
        Self {
            full,
            rows: if full {
                BTreeSet::new()
            } else {
                snapshot.changed_rows.iter().copied().collect()
            },
            cursor: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameStats {
    pub presented: u64,
    pub sync_timeouts: u64,
}

/// Times are offsets from a caller-owned monotonic epoch. Call `take_frame` from
/// a display callback or one-shot timer, never from an idle polling loop.
#[derive(Debug)]
pub struct FrameScheduler {
    interval: Duration,
    last_frame: Option<Duration>,
    sync_started: Option<Duration>,
    pending: FrameDamage,
    stats: FrameStats,
}

impl FrameScheduler {
    pub fn new(refresh_hz: u16) -> Result<Self, InvalidRefreshRate> {
        if refresh_hz == 0 {
            return Err(InvalidRefreshRate);
        }
        Ok(Self {
            interval: Duration::from_secs_f64(1.0 / f64::from(refresh_hz)),
            last_frame: None,
            sync_started: None,
            pending: FrameDamage::default(),
            stats: FrameStats::default(),
        })
    }

    pub fn request(&mut self, damage: FrameDamage) {
        self.pending.merge(damage);
    }

    pub fn begin_synchronized_output(&mut self, now: Duration) {
        // Repeated start sequences cannot keep the renderer frozen forever.
        self.sync_started.get_or_insert(now);
    }

    pub fn end_synchronized_output(&mut self) {
        self.sync_started = None;
    }

    pub fn next_deadline(&self) -> Option<Duration> {
        if self.pending.is_empty() {
            return None;
        }
        let pacing = self
            .last_frame
            .map(|last| last.saturating_add(self.interval))
            .unwrap_or_default();
        Some(
            self.sync_started
                .map(|start| pacing.max(start.saturating_add(SYNC_TIMEOUT)))
                .unwrap_or(pacing),
        )
    }

    pub fn take_frame(&mut self, now: Duration) -> Option<FrameDamage> {
        if now < self.next_deadline()? {
            return None;
        }
        if self.sync_started.take().is_some() {
            self.stats.sync_timeouts += 1;
        }
        self.last_frame = Some(now);
        self.stats.presented += 1;
        Some(std::mem::take(&mut self.pending))
    }

    pub fn stats(&self) -> FrameStats {
        self.stats
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidRefreshRate;

impl std::fmt::Display for InvalidRefreshRate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("display refresh rate must be nonzero")
    }
}

impl std::error::Error for InvalidRefreshRate {}

/// UI state shares the same input policy as the encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionPresentation {
    pub dimmed: bool,
    pub input_enabled: bool,
    pub message: Option<&'static str>,
}

impl From<SessionState> for SessionPresentation {
    fn from(state: SessionState) -> Self {
        Self {
            dimmed: !state.accepts_input(),
            input_enabled: state.accepts_input(),
            message: match state {
                SessionState::Connected => None,
                SessionState::Connecting => Some("Connecting…"),
                SessionState::Authenticating => Some("Signing in…"),
                SessionState::Disconnected => Some("Connection lost. Waiting for the network…"),
                SessionState::Reconnecting => Some("Reconnecting…"),
                SessionState::NeedsUserAction => Some("Your attention is needed to connect."),
                SessionState::Closed => Some("Session closed."),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(index: usize) -> FrameDamage {
        FrameDamage {
            rows: [index].into(),
            ..FrameDamage::default()
        }
    }

    #[test]
    fn idle_has_no_deadline_or_redraw() {
        let mut scheduler = FrameScheduler::new(120).unwrap();
        assert_eq!(scheduler.next_deadline(), None);
        assert_eq!(scheduler.take_frame(Duration::from_secs(100)), None);
        assert_eq!(scheduler.stats().presented, 0);
    }

    #[test]
    fn damage_accumulates_without_exceeding_refresh_rate() {
        let mut scheduler = FrameScheduler::new(120).unwrap();
        scheduler.request(row(1));
        assert!(scheduler.take_frame(Duration::ZERO).is_some());
        scheduler.request(row(2));
        scheduler.request(row(3));
        assert!(scheduler.take_frame(Duration::from_millis(1)).is_none());
        assert_eq!(
            scheduler.take_frame(Duration::from_millis(9)).unwrap().rows,
            [2, 3].into()
        );
        assert_eq!(scheduler.next_deadline(), None);
    }

    #[test]
    fn synchronized_update_presents_only_after_end() {
        let mut scheduler = FrameScheduler::new(60).unwrap();
        scheduler.begin_synchronized_output(Duration::ZERO);
        scheduler.request(row(1));
        assert!(scheduler.take_frame(Duration::from_millis(30)).is_none());
        scheduler.request(row(2));
        scheduler.end_synchronized_output();
        assert_eq!(
            scheduler
                .take_frame(Duration::from_millis(31))
                .unwrap()
                .rows,
            [1, 2].into()
        );
        assert_eq!(scheduler.stats().sync_timeouts, 0);
    }

    #[test]
    fn unterminated_sync_and_repeated_begin_cannot_freeze_display() {
        let mut scheduler = FrameScheduler::new(144).unwrap();
        scheduler.begin_synchronized_output(Duration::ZERO);
        scheduler.request(row(0));
        scheduler.begin_synchronized_output(Duration::from_millis(140));
        assert!(scheduler.take_frame(Duration::from_millis(149)).is_none());
        assert!(scheduler.take_frame(Duration::from_millis(150)).is_some());
        assert_eq!(scheduler.stats().sync_timeouts, 1);
        scheduler.request(row(1));
        assert!(scheduler.take_frame(Duration::from_millis(160)).is_some());
    }

    #[test]
    fn full_damage_supersedes_individual_rows() {
        let mut scheduler = FrameScheduler::new(60).unwrap();
        scheduler.request(row(5));
        scheduler.request(FrameDamage {
            full: true,
            ..FrameDamage::default()
        });
        scheduler.request(row(6));
        let damage = scheduler.take_frame(Duration::ZERO).unwrap();
        assert!(damage.full);
        assert!(damage.rows.is_empty());
    }

    #[test]
    fn cursor_damage_schedules_a_frame_but_no_permanent_timer() {
        let mut scheduler = FrameScheduler::new(60).unwrap();
        scheduler.request(FrameDamage {
            cursor: true,
            ..FrameDamage::default()
        });
        assert!(scheduler.take_frame(Duration::ZERO).unwrap().cursor);
        assert!(scheduler.next_deadline().is_none());
        assert!(FrameScheduler::new(0).is_err());
    }

    #[test]
    fn disconnected_presentation_matches_encoder_policy() {
        let presentation = SessionPresentation::from(SessionState::Disconnected);
        assert!(presentation.dimmed);
        assert!(!presentation.input_enabled);
        assert!(presentation.message.is_some());
    }
}
