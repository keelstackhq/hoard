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

/// Shared by all request handlers. Open until a rate limit closes it.
#[derive(Debug, Default)]
pub struct RateGate {
    /// Uploads are skipped before this instant.
    paused_until: Mutex<Option<Instant>>,
}

impl RateGate {
    /// True when an upload may go to the store at `now`.
    pub fn allow_upload(&self, now: Instant) -> bool {
        match *self.lock() {
            Some(until) => now >= until,
            None => true,
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
