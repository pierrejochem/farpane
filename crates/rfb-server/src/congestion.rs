//! How much a client may have in flight: a byte window that follows the
//! link, so a fast link is filled and a slow one is not flooded.
//!
//! The client confirms each update with a fence, and the time between the
//! update and its confirmation is a round trip plus whatever waited in
//! queues on the way. The shortest round trip seen is the empty-queue
//! baseline. While round trips stay near it the window grows (doubling at
//! first, then by a few KiB per confirmation); once they stretch the link is
//! saturated, and the window drops to what the link was seen to carry in one
//! baseline round trip. The idea follows `TigerVNC`'s congestion control, in a
//! much smaller form.

use std::time::Duration;

/// Where the window starts: enough for a good network, quickly cut if not.
const INITIAL_WINDOW: usize = 16 * 1024;
/// The window never shrinks below this, so an update can always be in flight.
const MIN_WINDOW: usize = 4 * 1024;
const MAX_WINDOW: usize = 64 * 1024 * 1024;
/// A round trip this much over the baseline (or 20 ms, if more) counts as
/// queueing.
const SLACK_PERCENT: u32 = 50;
const SLACK_MIN: Duration = Duration::from_millis(20);
/// Growth per confirmation after slow start.
const STEP: usize = 4 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct Congestion {
    window: usize,
    base_rtt: Option<Duration>,
    slow_start: bool,
}

impl Congestion {
    pub(crate) fn new() -> Self {
        Self {
            window: INITIAL_WINDOW,
            base_rtt: None,
            slow_start: true,
        }
    }

    /// Bytes that may be unconfirmed before the next update waits.
    pub(crate) fn window(&self) -> usize {
        self.window
    }

    /// An update was confirmed `rtt` after it was sent; `acked` bytes were
    /// confirmed by this answer (it covers older updates too).
    pub(crate) fn confirmed(&mut self, rtt: Duration, acked: usize) {
        let rtt = rtt.max(Duration::from_micros(1));
        let base = *self.base_rtt.get_or_insert(rtt);
        let base = base.min(rtt);
        self.base_rtt = Some(base);
        let slack = (base * SLACK_PERCENT / 100).max(SLACK_MIN);
        if rtt <= base + slack {
            // No queue: room to grow.
            self.window = if self.slow_start {
                self.window.saturating_mul(2)
            } else {
                self.window.saturating_add(STEP)
            }
            .min(MAX_WINDOW);
        } else {
            // Queue: what the link carried in one baseline round trip, never
            // more than the window was.
            self.slow_start = false;
            let carried = (acked as u128 * base.as_micros().max(1)) / rtt.as_micros().max(1);
            self.window = usize::try_from(carried)
                .unwrap_or(usize::MAX)
                .clamp(MIN_WINDOW, self.window);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn a_quiet_link_doubles_the_window_at_first() {
        let mut c = Congestion::new();
        assert_eq!(c.window(), INITIAL_WINDOW);
        c.confirmed(5 * MS, 1000);
        assert_eq!(c.window(), INITIAL_WINDOW * 2);
        c.confirmed(5 * MS, 1000);
        assert_eq!(c.window(), INITIAL_WINDOW * 4);
    }

    #[test]
    fn a_stretched_round_trip_cuts_the_window_to_what_the_link_carried() {
        let mut c = Congestion::new();
        for _ in 0..4 {
            c.confirmed(5 * MS, 1000);
        }
        let before = c.window();
        // 200 KB confirmed after 100 ms against a 5 ms baseline: 2 MB/s, so
        // 10 KB fit in one baseline round trip.
        c.confirmed(100 * MS, 200_000);
        assert!(c.window() < before);
        assert!((9_000..=11_000).contains(&c.window()), "{}", c.window());
        // Slow start is over: growth is now a step per confirmation.
        let low = c.window();
        c.confirmed(5 * MS, 1000);
        assert_eq!(c.window(), low + STEP);
    }

    #[test]
    fn the_window_stays_between_its_limits() {
        let mut c = Congestion::new();
        for _ in 0..40 {
            c.confirmed(MS, 1);
        }
        assert_eq!(c.window(), MAX_WINDOW);
        let mut c = Congestion::new();
        c.confirmed(MS, 1);
        c.confirmed(900 * MS, 1);
        assert_eq!(c.window(), MIN_WINDOW);
    }

    #[test]
    fn the_baseline_follows_the_shortest_round_trip() {
        let mut c = Congestion::new();
        c.confirmed(50 * MS, 1000);
        // A faster one lowers the baseline: 50 ms is now a queue.
        c.confirmed(5 * MS, 1000);
        let before = c.window();
        c.confirmed(50 * MS, 1000);
        assert!(c.window() < before);
    }
}
