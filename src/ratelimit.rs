//! Upload pause after the storage backend rate-limits us (GitHub cache: HTTP 429).
//!
//! The GitHub cache allows about 200 uploads a minute per repository. After a 429 every upload
//! is skipped (answered `200`, counted as `rate_limited`) until `Retry-After` has passed;
//! reads are never paused.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::warn;

/// Pause used when the backend gives no `Retry-After`.
pub const DEFAULT_PAUSE: Duration = Duration::from_secs(60);

/// Longest pause honoured. A larger `Retry-After` (a bogus or hostile value) would otherwise
/// overflow `Instant` arithmetic or silently disable uploads for the rest of the job; GitHub's
/// limits are per minute, so 10 minutes is plenty.
pub const MAX_PAUSE: Duration = Duration::from_secs(10 * 60);

/// Consecutive read errors (timeouts, 5xx) that open the read breaker.
const READ_FAILS: u32 = 3;

/// How long reads are answered as misses once the breaker is open. A dead backend would
/// otherwise cost every task one full request timeout (Turborepo waits up to 30 s each).
pub const READ_BREAK: Duration = Duration::from_secs(30);

/// Shared by all request handlers. Open until a rate limit closes it.
#[derive(Debug, Default)]
pub struct RateGate {
    /// Uploads are skipped before this instant.
    paused_until: Mutex<Option<Instant>>,
    /// Read breaker: (consecutive errors, reads skipped before this instant).
    reads: Mutex<(u32, Option<Instant>)>,
}

impl RateGate {
    /// True when an upload may go to the store at `now`.
    pub fn allow_upload(&self, now: Instant) -> bool {
        match *self.lock() {
            Some(until) => now >= until,
            None => true,
        }
    }

    /// How long uploads stay paused after `now` (zero when the gate is open).
    pub fn remaining_pause(&self, now: Instant) -> Duration {
        match *self.lock() {
            Some(until) => until.saturating_duration_since(now),
            None => Duration::ZERO,
        }
    }

    /// The backend rate-limited an upload at `now`: pause uploads for `retry_after` (default
    /// [`DEFAULT_PAUSE`], at most [`MAX_PAUSE`]). A pause is only ever extended, never
    /// shortened. Logs one warning
    /// when this closes an open gate.
    pub fn on_rate_limited(&self, now: Instant, retry_after: Option<Duration>) {
        let pause = retry_after.unwrap_or(DEFAULT_PAUSE).min(MAX_PAUSE);
        // Cannot overflow for any real clock; if it ever did, pause by the default instead.
        let until = now
            .checked_add(pause)
            .or_else(|| now.checked_add(DEFAULT_PAUSE))
            .unwrap_or(now);
        let mut slot = self.lock();
        let was_open = slot.is_none_or(|u| now >= u);
        if slot.is_none_or(|u| until > u) {
            *slot = Some(until);
        }
        drop(slot);
        if was_open {
            warn!(
                pause_secs = pause.as_secs(),
                "storage rate limit hit: skipping uploads for {}s (reads continue)",
                pause.as_secs()
            );
        }
    }

    /// True when a read may go to the store. After [`READ_FAILS`] errors in a row reads miss
    /// at once for [`READ_BREAK`]; the first read after that probes the backend again.
    pub fn allow_read(&self, now: Instant) -> bool {
        self.reads_lock().1.is_none_or(|until| now >= until)
    }

    /// A read reached the store and got an answer (hit or miss).
    pub fn read_ok(&self) {
        *self.reads_lock() = (0, None);
    }

    /// A read failed with a storage error (not a miss). Logs once when the breaker opens.
    pub fn read_failed(&self, now: Instant) {
        let mut r = self.reads_lock();
        r.0 += 1;
        if r.0 >= READ_FAILS {
            r.0 = READ_FAILS - 1; // one failed probe reopens it
            r.1 = now.checked_add(READ_BREAK);
            drop(r);
            warn!(
                "storage reads keep failing: answering reads as misses for {}s",
                READ_BREAK.as_secs()
            );
        }
    }

    fn reads_lock(&self) -> std::sync::MutexGuard<'_, (u32, Option<Instant>)> {
        self.reads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Instant>> {
        // The guarded value is a plain Instant; a panic elsewhere cannot leave it inconsistent.
        self.paused_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn read_breaker_opens_after_three_failures_and_closes_on_success() {
        let g = RateGate::default();
        let t0 = Instant::now();
        assert!(g.allow_read(t0));
        g.read_failed(t0);
        g.read_failed(t0);
        assert!(g.allow_read(t0), "two failures are not an outage");
        g.read_ok();
        g.read_failed(t0);
        g.read_failed(t0);
        assert!(g.allow_read(t0), "a success resets the count");
        g.read_failed(t0);
        assert!(!g.allow_read(t0 + READ_BREAK - Duration::from_secs(1)));
        assert!(g.allow_read(t0 + READ_BREAK), "probe after the pause");
    }

    #[test]
    fn gate_pauses_and_resumes() {
        let g = RateGate::default();
        let t0 = Instant::now();
        assert!(g.allow_upload(t0));
        g.on_rate_limited(t0, Some(Duration::from_secs(5)));
        assert!(!g.allow_upload(t0 + Duration::from_secs(4)));
        assert!(g.allow_upload(t0 + Duration::from_secs(6)));
        g.on_rate_limited(t0, None);
        assert!(!g.allow_upload(t0 + Duration::from_secs(59)));
    }

    #[test]
    fn default_pause_is_60s_and_shorter_retry_does_not_shorten() {
        let g = RateGate::default();
        let t0 = Instant::now();
        g.on_rate_limited(t0, None);
        assert!(!g.allow_upload(t0 + Duration::from_millis(59_999)));
        assert!(g.allow_upload(t0 + Duration::from_secs(60)));
        let g = RateGate::default();
        g.on_rate_limited(t0, Some(Duration::from_secs(30)));
        g.on_rate_limited(t0, Some(Duration::from_secs(1)));
        assert!(!g.allow_upload(t0 + Duration::from_secs(29)));
        assert!(g.allow_upload(t0 + Duration::from_secs(30)));
    }

    #[test]
    fn remaining_pause_counts_down_to_zero() {
        let g = RateGate::default();
        let t0 = Instant::now();
        assert_eq!(g.remaining_pause(t0), Duration::ZERO);
        g.on_rate_limited(t0, Some(Duration::from_secs(5)));
        assert_eq!(
            g.remaining_pause(t0 + Duration::from_secs(2)),
            Duration::from_secs(3)
        );
        assert_eq!(
            g.remaining_pause(t0 + Duration::from_secs(9)),
            Duration::ZERO
        );
    }

    #[test]
    fn huge_retry_after_is_clamped_without_panic() {
        let g = RateGate::default();
        let t0 = Instant::now();
        g.on_rate_limited(t0, Some(Duration::from_secs(u64::MAX)));
        assert!(!g.allow_upload(t0 + MAX_PAUSE - Duration::from_secs(1)));
        assert!(g.allow_upload(t0 + MAX_PAUSE));
    }

    #[test]
    fn instants_before_the_gate_was_built_are_fine() {
        let early = Instant::now();
        let g = RateGate::default();
        assert!(g.allow_upload(early));
        g.on_rate_limited(early, Some(Duration::from_secs(5)));
        assert!(!g.allow_upload(early + Duration::from_secs(1)));
    }
}
