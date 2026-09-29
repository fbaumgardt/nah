//! Shared exponential reconnect backoff for the action and event connections.
//!
//! Both connections to the compositor retry on their own schedule, so the
//! parameters live here once: first retry after [`INITIAL_BACKOFF`], doubling
//! each failure up to [`MAX_BACKOFF`]. A successful connection calls
//! [`Backoff::reset`] so the next drop starts over at 200 ms.

use std::time::Duration;

/// First retry delay after a connection failure.
pub const INITIAL_BACKOFF: Duration = Duration::from_millis(200);

/// Upper bound for the retry delay.
pub const MAX_BACKOFF: Duration = Duration::from_secs(5);

/// Exponential backoff schedule: 200 ms, 400 ms, 800 ms, … capped at 5 s.
#[derive(Debug, Clone)]
pub struct Backoff {
    delay: Duration,
}

impl Backoff {
    /// Start a fresh schedule at [`INITIAL_BACKOFF`].
    pub const fn new() -> Self {
        Self {
            delay: INITIAL_BACKOFF,
        }
    }

    /// The delay [`Backoff::next_delay`] would return next, without advancing.
    pub const fn peek(&self) -> Duration {
        self.delay
    }

    /// Return the delay to wait before the next attempt and advance the
    /// schedule (`×2`, capped at [`MAX_BACKOFF`]).
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.delay;
        self.delay = (delay * 2).min(MAX_BACKOFF);
        delay
    }

    /// Start over at [`INITIAL_BACKOFF`] after a successful connection.
    pub fn reset(&mut self) {
        self.delay = INITIAL_BACKOFF;
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_doubles_then_caps() {
        let mut backoff = Backoff::new();
        let expected = [200, 400, 800, 1600, 3200, 5000, 5000, 5000];
        for millis in expected {
            assert_eq!(backoff.peek(), Duration::from_millis(millis));
            assert_eq!(backoff.next_delay(), Duration::from_millis(millis));
        }
    }

    #[test]
    fn reset_returns_to_the_initial_delay() {
        let mut backoff = Backoff::new();
        for _ in 0..6 {
            let _ = backoff.next_delay();
        }
        assert_eq!(backoff.peek(), MAX_BACKOFF);
        backoff.reset();
        assert_eq!(backoff.peek(), INITIAL_BACKOFF);
        assert_eq!(backoff.next_delay(), INITIAL_BACKOFF);
    }
}
