//! Round-trip and one-way delay estimates, the way Syncplay makes them.
//!
//! Both ends of a connection run one of these. Each side stamps the messages
//! it sends with its own clock, the other side echoes the stamp back, and the
//! difference is a round trip measured entirely on one clock — so the two
//! machines never have to agree on what time it is.
//!
//! The forward delay is what matters for playback: it is how old a position
//! report already is by the time it arrives, and a playing film has moved on
//! by that much. Ported line for line from upstream's `PingService`, because
//! two peers that disagree about this estimate disagree about where the room
//! is.

/// How much of the running average survives each new sample. Upstream's
/// `PING_MOVING_AVERAGE_WEIGHT`.
const MOVING_AVERAGE_WEIGHT: f64 = 0.85;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PingService {
    rtt: f64,
    average_rtt: f64,
    forward_delay: f64,
}

impl PingService {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes one echoed timestamp.
    ///
    /// `timestamp` is a stamp this side sent earlier, now coming back;
    /// `sender_rtt` is the round trip the other side last measured, so the
    /// asymmetry between the two directions can be accounted for. `now_epoch`
    /// is this side's clock, in the same seconds-since-the-epoch the stamp was
    /// taken in.
    ///
    /// A missing or zero stamp is ignored, as upstream ignores it. The raw
    /// round trip is recorded before the sign check, again as upstream does,
    /// so a clock that stepped backwards shows up in [`Self::rtt`] but never
    /// reaches the average.
    pub fn receive(&mut self, timestamp: Option<f64>, sender_rtt: f64, now_epoch: f64) {
        let Some(timestamp) = timestamp.filter(|stamp| *stamp != 0.0) else {
            return;
        };

        self.rtt = now_epoch - timestamp;
        if self.rtt < 0.0 || sender_rtt < 0.0 {
            return;
        }

        if self.average_rtt == 0.0 {
            self.average_rtt = self.rtt;
        }
        self.average_rtt =
            self.average_rtt * MOVING_AVERAGE_WEIGHT + self.rtt * (1.0 - MOVING_AVERAGE_WEIGHT);

        self.forward_delay = if sender_rtt < self.rtt {
            self.average_rtt / 2.0 + (self.rtt - sender_rtt)
        } else {
            self.average_rtt / 2.0
        };
    }

    /// The last raw round trip, in seconds. Can be negative after a clock
    /// step; anything that puts it on the wire should clamp it at zero.
    pub fn rtt(&self) -> f64 {
        self.rtt
    }

    pub fn average_rtt(&self) -> f64 {
        self.average_rtt
    }

    /// How long a message from the other side is estimated to have been in
    /// flight, in seconds. Zero until a round trip has been measured.
    pub fn forward_delay(&self) -> f64 {
        self.forward_delay
    }
}

/// Seconds since the Unix epoch, which is the unit every Syncplay timestamp
/// is in.
pub fn epoch_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn a_fresh_service_estimates_no_delay() {
        let ping = PingService::new();

        assert_eq!(ping.rtt(), 0.0);
        assert_eq!(ping.forward_delay(), 0.0);
    }

    #[test]
    fn a_missing_or_zero_stamp_is_ignored() {
        let mut ping = PingService::new();

        ping.receive(None, 0.0, 100.0);
        ping.receive(Some(0.0), 0.0, 100.0);

        assert_eq!(ping, PingService::new());
    }

    #[test]
    fn the_first_sample_seeds_the_average() {
        let mut ping = PingService::new();

        ping.receive(Some(99.8), 0.0, 100.0);

        assert!(close(ping.rtt(), 0.2));
        assert!(close(ping.average_rtt(), 0.2));
        // The other side has measured nothing yet, so the whole of this round
        // trip beyond the average half is attributed to the forward path.
        assert!(close(ping.forward_delay(), 0.1 + 0.2));
    }

    #[test]
    fn later_samples_move_the_average_by_fifteen_percent() {
        let mut ping = PingService::new();
        ping.receive(Some(99.8), 0.2, 100.0);
        ping.receive(Some(199.6), 0.2, 200.0);

        assert!(close(ping.average_rtt(), 0.2 * 0.85 + 0.4 * 0.15));
    }

    #[test]
    fn a_symmetric_link_puts_half_the_average_on_each_side() {
        let mut ping = PingService::new();

        ping.receive(Some(99.8), 0.2, 100.0);

        assert!(close(ping.forward_delay(), 0.1));
    }

    #[test]
    fn a_clock_step_is_recorded_but_never_averaged() {
        let mut ping = PingService::new();
        ping.receive(Some(99.8), 0.2, 100.0);

        ping.receive(Some(101.0), 0.2, 100.0);

        assert!(
            ping.rtt() < 0.0,
            "the raw reading is kept, as upstream keeps it"
        );
        assert!(close(ping.average_rtt(), 0.2));
        assert!(close(ping.forward_delay(), 0.1));
    }
}
