//! The per-connection `ignoringOnTheFly` handshake.
//!
//! The counters are per-connection rather than per-room, and that is the whole
//! correction this module exists to hold. Each client acknowledges a force
//! independently; a server that shared one counter across a room would let one
//! lagging person silence everybody else.

/// Whether this connection's reports may still be believed.
#[derive(Default)]
pub struct IgnoreGate {
    /// Bumped on every forced send, cleared when the client acknowledges that
    /// exact value. Non-zero means a force is still in flight.
    server: u64,
    /// The client's own counter, waiting to be echoed back to it.
    client: u64,
}

impl IgnoreGate {
    /// Reads the `ignoringOnTheFly` object off an incoming message.
    pub fn observe(&mut self, server_ack: Option<u64>, client_value: Option<u64>) {
        if server_ack == Some(self.server) {
            self.server = 0;
        }

        // Only an actual value replaces the stored one. A message that carries
        // no client half is silent about it rather than cancelling an echo
        // that has not gone out yet.
        if let Some(value) = client_value {
            self.client = value;
        }
    }

    /// Whether this connection's reports describe the world as it is now.
    ///
    /// While a force is unacknowledged everything in flight describes the world
    /// before it, so it is dropped rather than applied.
    pub fn accepts_updates(&self) -> bool {
        self.server == 0
    }

    pub fn on_forced_send(&mut self) {
        self.server += 1;
    }

    /// The `server`/`client` pair to attach to an outgoing message, if any.
    ///
    /// The client half is echoed exactly once; the server half stays until the
    /// client acknowledges it.
    pub fn take_envelope(&mut self) -> Option<(Option<u64>, Option<u64>)> {
        if self.server == 0 && self.client == 0 {
            return None;
        }

        let client = (self.client != 0).then_some(self.client);
        self.client = 0;

        Some(((self.server != 0).then_some(self.server), client))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_gate_accepts_updates() {
        assert!(IgnoreGate::default().accepts_updates());
    }

    #[test]
    fn a_forced_send_closes_the_gate_until_the_client_catches_up() {
        let mut gate = IgnoreGate::default();
        gate.on_forced_send();

        assert!(
            !gate.accepts_updates(),
            "everything in flight describes the world before the force"
        );

        gate.observe(Some(1), None);
        assert!(
            gate.accepts_updates(),
            "the echo means the client has caught up"
        );
    }

    #[test]
    fn an_echo_of_the_wrong_counter_does_not_open_the_gate() {
        let mut gate = IgnoreGate::default();
        gate.on_forced_send();
        gate.on_forced_send();

        gate.observe(Some(1), None);

        assert!(!gate.accepts_updates(), "that acknowledges an older force");
    }

    #[test]
    fn the_client_half_is_echoed_exactly_once() {
        let mut gate = IgnoreGate::default();
        gate.observe(None, Some(7));

        assert_eq!(gate.take_envelope(), Some((None, Some(7))));
        assert_eq!(gate.take_envelope(), None, "echoed once, then forgotten");
    }

    #[test]
    fn a_quiet_gate_attaches_nothing() {
        assert_eq!(IgnoreGate::default().take_envelope(), None);
    }
}
