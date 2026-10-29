//! TUI liveness budgets. These constants are the source of truth for UI-only
//! LIST probes and reconnect backoff; no terminal wire or service timer changes.
use std::time::{Duration, Instant};

const PROBE_INTERVAL: Duration = Duration::from_secs(5);
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const ATTACH_TIMEOUT: Duration = Duration::from_secs(15);
const RETRY_DELAYS: [Duration; 5] = [
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
];

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Action {
    None,
    Probe,
    Reconnect,
    TimedOut,
}

struct Probe {
    deadline: Instant,
    queued: bool,
}

pub(super) struct Recovery {
    failures: usize,
    retry_at: Option<Instant>,
    attach_deadline: Option<Instant>,
    next_probe: Instant,
    probe: Option<Probe>,
}

impl Recovery {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            failures: 0,
            retry_at: None,
            attach_deadline: Some(now + ATTACH_TIMEOUT),
            next_probe: now,
            probe: None,
        }
    }

    pub(super) fn begin(&mut self, now: Instant) {
        self.retry_at = None;
        self.attach_deadline = Some(now + ATTACH_TIMEOUT);
        self.next_probe = now;
        self.probe = None;
    }

    pub(super) fn ready(&mut self) {
        self.failures = 0;
        self.attach_deadline = None;
    }

    pub(super) fn retrying(&self) -> bool {
        self.failures != 0
    }

    pub(super) fn fail(&mut self, now: Instant) -> Duration {
        let delay = RETRY_DELAYS[self.failures.min(RETRY_DELAYS.len() - 1)];
        self.failures = self.failures.saturating_add(1);
        self.retry_at = Some(now + delay);
        self.attach_deadline = None;
        self.probe = None;
        delay
    }

    pub(super) fn poll(&mut self, now: Instant) -> Action {
        if let Some(at) = self.retry_at {
            return if now >= at {
                Action::Reconnect
            } else {
                Action::None
            };
        }
        if self.expired(now) {
            return Action::TimedOut;
        }
        if self.probe.is_none() && now >= self.next_probe {
            // Start at the first enqueue attempt: a perpetually busy writer
            // cannot postpone the liveness deadline.
            self.probe = Some(Probe {
                deadline: now + PROBE_TIMEOUT,
                queued: false,
            });
        }
        if self.probe.as_ref().is_some_and(|probe| !probe.queued) {
            Action::Probe
        } else {
            Action::None
        }
    }

    pub(super) fn expired(&self, now: Instant) -> bool {
        self.retry_at.is_none()
            && (self.attach_deadline.is_some_and(|at| now >= at)
                || self
                    .probe
                    .as_ref()
                    .is_some_and(|probe| now >= probe.deadline))
    }

    pub(super) fn queued(&mut self) {
        if let Some(probe) = &mut self.probe {
            probe.queued = true;
        }
    }

    pub(super) fn expects_reply(&self, now: Instant) -> bool {
        self.probe
            .as_ref()
            .is_some_and(|probe| probe.queued && now < probe.deadline)
    }

    pub(super) fn replied(&mut self, now: Instant) {
        self.probe = None;
        self.next_probe = now + PROBE_INTERVAL;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_probe_is_pending_and_busy_cannot_extend_its_deadline() {
        let now = Instant::now();
        let mut recovery = Recovery::new(now);
        assert_eq!(recovery.poll(now), Action::Probe);
        assert_eq!(recovery.poll(now + Duration::from_secs(9)), Action::Probe);
        assert_eq!(recovery.poll(now + PROBE_TIMEOUT), Action::TimedOut);
        recovery.begin(now);
        assert_eq!(recovery.poll(now), Action::Probe);
        recovery.queued();
        assert_eq!(recovery.poll(now + Duration::from_secs(9)), Action::None);
        assert!(recovery.expects_reply(now + Duration::from_secs(9)));
        assert!(!recovery.expects_reply(now + PROBE_TIMEOUT));
        assert_eq!(recovery.poll(now + PROBE_TIMEOUT), Action::TimedOut);
    }

    #[test]
    fn idle_replies_keep_connection_healthy_but_cannot_replace_initial_snapshot() {
        let now = Instant::now();
        let mut recovery = Recovery::new(now);
        for seconds in [0, 5, 10] {
            let at = now + Duration::from_secs(seconds);
            assert_eq!(recovery.poll(at), Action::Probe);
            recovery.queued();
            recovery.replied(at);
        }
        assert!(recovery.expired(now + ATTACH_TIMEOUT));
        recovery.begin(now);
        recovery.ready();
        for seconds in (0..=60).step_by(5) {
            let at = now + Duration::from_secs(seconds);
            assert_eq!(recovery.poll(at), Action::Probe);
            recovery.queued();
            recovery.replied(at);
            assert!(!recovery.expired(at));
        }
    }

    #[test]
    fn backoff_is_capped_and_resets_only_after_complete_success() {
        let now = Instant::now();
        let mut recovery = Recovery::new(now);
        for expected in RETRY_DELAYS.into_iter().chain([Duration::from_secs(8); 2]) {
            assert_eq!(recovery.fail(now), expected);
            assert_eq!(
                recovery.poll(now + expected - Duration::from_millis(1)),
                Action::None
            );
            assert_eq!(recovery.poll(now + expected), Action::Reconnect);
            recovery.begin(now + expected);
            assert!(recovery.retrying());
        }
        recovery.ready();
        assert!(!recovery.retrying());
        assert_eq!(recovery.fail(now), Duration::from_millis(500));
    }
}
