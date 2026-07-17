//! The single event funnel and the deadline-min timer set.
//!
//! Every source the loop selects over — terminal events, internal channel
//! messages, timer deadlines — maps into one [`LoopEvent`] before any state
//! is touched. New sources (context events, subprocess output) add a
//! variant here, never a second dispatch path.

use tokio::time::Instant;

/// Everything the loop can wake on, unified.
#[derive(Debug)]
pub enum LoopEvent {
    /// A terminal input/resize event from crossterm.
    Term(crossterm::event::Event),
    /// An armed deadline fired.
    Timer(TimerId),
    /// An internal event from a background task.
    App(AppEvent),
}

/// Internal events background tasks send into the loop's channel.
#[derive(Debug)]
pub enum AppEvent {
    /// The demo job settled: `Ok` clears to idle, `Err` sticks as an error.
    WorkFinished(Result<(), String>),
}

/// Identity of an armed timer. One id = one logical timer; re-arming an id
/// replaces its deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerId {
    /// Spinner frame advance while a job runs.
    SpinnerTick,
}

/// Deadline-min timer set: the loop sleeps until the *earliest* armed
/// deadline, and pops every due timer on wake. Linear scan — the set holds
/// a handful of logical timers, never per-item deadlines.
#[derive(Debug, Default)]
pub struct Deadlines {
    armed: Vec<(TimerId, Instant)>,
}

impl Deadlines {
    /// Arm `id` for `at`, replacing any existing deadline for the same id.
    pub fn arm(&mut self, id: TimerId, at: Instant) {
        self.disarm(id);
        self.armed.push((id, at));
    }

    pub fn disarm(&mut self, id: TimerId) {
        self.armed.retain(|(armed_id, _)| *armed_id != id);
    }

    /// The earliest armed deadline, if any — what the loop sleeps until.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.armed.iter().map(|(_, at)| *at).min()
    }

    /// Remove and return every timer due at `now`.
    pub fn pop_due(&mut self, now: Instant) -> Vec<TimerId> {
        let (due, pending): (Vec<_>, Vec<_>) = self.armed.drain(..).partition(|(_, at)| *at <= now);
        self.armed = pending;
        due.into_iter().map(|(id, _)| id).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn rearming_replaces_and_min_wins() {
        let now = Instant::now();
        let mut d = Deadlines::default();
        d.arm(TimerId::SpinnerTick, now + Duration::from_secs(10));
        d.arm(TimerId::SpinnerTick, now + Duration::from_secs(1));
        assert_eq!(d.next_deadline(), Some(now + Duration::from_secs(1)));
        assert!(d.pop_due(now).is_empty());
        let due = d.pop_due(now + Duration::from_secs(2));
        assert_eq!(due, vec![TimerId::SpinnerTick]);
        assert_eq!(d.next_deadline(), None);
    }
}
