//! Hears what the player tells the sync client.
//!
//! The sync client takes the player's event stream for itself, and the watch
//! session needs the same events to know when a file opened or the window
//! closed. So the stream is split here: every event goes to the client and
//! then to the session, in the order the player sent them.
//!
//! The split must not break the promise the client is built on. A player
//! queues an event before the change it reports can be seen in its snapshot,
//! and the client relies on that: it looks at the snapshot, then at its
//! queue, and an empty queue means nothing in the snapshot is news. A task
//! forwarding events in the background would break it, because the snapshot
//! could show a change whose event is still waiting to be forwarded. So the
//! tap's own snapshot forwards everything already queued before it returns,
//! and the event of any change the snapshot shows is in the client's queue by
//! then.

use std::future::poll_fn;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::Poll;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::core::error::Result;
use crate::core::player::{CloseReason, Player, PlayerEvent, PlayerEvents, PlayerSnapshot};

/// One player's events, tagged with which launch they came from, on their
/// way to the watch session.
pub(super) type Heard = mpsc::UnboundedSender<(u64, PlayerEvent)>;

/// A player with its event stream split in two. Hand the tap to the client
/// as its player; everything else it does is the real player's.
pub(super) struct Tap {
    player: Arc<dyn Player>,
    generation: u64,
    route: Mutex<Route>,
}

struct Route {
    source: PlayerEvents,
    /// Dropped once the player has gone, which ends the client's stream.
    client: Option<mpsc::UnboundedSender<PlayerEvent>>,
    session: Heard,
    closed: bool,
}

impl Tap {
    /// Wraps `player`, returning the tap and the stream to give the client
    /// alongside it.
    pub(super) fn new(
        player: Arc<dyn Player>,
        source: PlayerEvents,
        generation: u64,
        session: Heard,
    ) -> (Arc<Self>, PlayerEvents) {
        let (client, events) = mpsc::unbounded_channel();
        let tap = Arc::new(Self {
            player,
            generation,
            route: Mutex::new(Route {
                source,
                client: Some(client),
                session,
                closed: false,
            }),
        });

        (tap, events)
    }

    fn lock(&self) -> MutexGuard<'_, Route> {
        // Nothing panics while holding it; a poisoned lock still guards a
        // usable queue.
        self.route
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The player's snapshot, without forwarding anything. For the watch
    /// session, which hears the events through its own channel anyway.
    pub(super) fn peek(&self) -> PlayerSnapshot {
        self.player.snapshot()
    }

    /// Whether the player has said it is gone.
    pub(super) fn is_closed(&self) -> bool {
        self.lock().closed
    }
}

impl Route {
    fn forward(&mut self, generation: u64, event: PlayerEvent) {
        let closing = matches!(event, PlayerEvent::Closed(_));

        // The client first: by the time the session acts on an event, the
        // client has it too.
        if let Some(client) = &self.client {
            let _ = client.send(event.clone());
        }
        let _ = self.session.send((generation, event));

        if closing {
            self.closed = true;
            self.client = None;
        }
    }

    fn drain(&mut self, generation: u64) {
        while !self.closed {
            let Ok(event) = self.source.try_recv() else {
                return;
            };
            self.forward(generation, event);
        }
    }

    /// The player's stream ended without a word, which only happens when the
    /// player went with it. The client reads the end of its own stream that
    /// way; the session is told in so many words.
    fn ended(&mut self, generation: u64) {
        if !self.closed {
            self.client = None;
            let _ = self
                .session
                .send((generation, PlayerEvent::Closed(CloseReason::Crashed(None))));
            self.closed = true;
        }
    }
}

/// Forwards events as they arrive, for as long as the player is open.
///
/// The lock is taken only for each look at the queue and never held across
/// a wait, so the client's snapshots can still forward in between.
pub(super) async fn pump(tap: Arc<Tap>) {
    let generation = tap.generation;

    loop {
        let open = poll_fn(|context| {
            let mut route = tap.lock();
            if route.closed {
                return Poll::Ready(false);
            }

            match route.source.poll_recv(context) {
                Poll::Ready(Some(event)) => {
                    route.forward(generation, event);
                    route.drain(generation);
                    Poll::Ready(!route.closed)
                }
                Poll::Ready(None) => {
                    route.ended(generation);
                    Poll::Ready(false)
                }
                Poll::Pending => Poll::Pending,
            }
        })
        .await;

        if !open {
            return;
        }
    }
}

#[async_trait]
impl Player for Tap {
    /// Forwards whatever the player queued before this look, so the client
    /// never sees a change ahead of its event.
    fn snapshot(&self) -> PlayerSnapshot {
        let snapshot = self.player.snapshot();
        self.lock().drain(self.generation);
        snapshot
    }

    async fn set_paused(&self, paused: bool) -> Result<()> {
        self.player.set_paused(paused).await
    }

    async fn seek(&self, position: f64) -> Result<()> {
        self.player.seek(position).await
    }

    async fn set_speed(&self, speed: f64) -> Result<()> {
        self.player.set_speed(speed).await
    }

    async fn load(&self, path: &Path) -> Result<()> {
        self.player.load(path).await
    }

    async fn show_text(&self, text: &str, duration: Duration) -> Result<()> {
        self.player.show_text(text, duration).await
    }

    async fn quit(&self) -> Result<()> {
        self.player.quit().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::player::fake::FakePlayer;
    use crate::core::player::Origin;

    /// A fake behind a tap, with both ends of the split to read.
    struct Tapped {
        player: Arc<FakePlayer>,
        tap: Arc<Tap>,
        client: PlayerEvents,
        heard: mpsc::UnboundedReceiver<(u64, PlayerEvent)>,
    }

    fn tapped() -> Tapped {
        let (player, source) = FakePlayer::new();
        let (session, heard) = mpsc::unbounded_channel();
        let (tap, client) = Tap::new(player.clone(), source, 7, session);
        Tapped {
            player,
            tap,
            client,
            heard,
        }
    }

    #[tokio::test]
    async fn a_change_in_the_snapshot_already_has_its_event_in_the_clients_queue() {
        let Tapped {
            player,
            tap,
            mut client,
            mut heard,
        } = tapped();

        // No pump running: only the snapshot can have forwarded this.
        player.user_pauses(false);
        assert!(!tap.snapshot().paused);

        assert_eq!(
            client.try_recv().ok(),
            Some(PlayerEvent::PauseChanged {
                paused: false,
                origin: Origin::External
            })
        );
        assert!(matches!(
            heard.try_recv(),
            Ok((7, PlayerEvent::PauseChanged { paused: false, .. }))
        ));
    }

    #[tokio::test]
    async fn the_pump_forwards_in_order_and_stops_once_the_player_has_gone() {
        let Tapped {
            player,
            tap,
            mut client,
            mut heard,
        } = tapped();
        let pump = tokio::spawn(pump(Arc::clone(&tap)));

        player.user_opens("/films/Film.mkv", Some(60.0));
        player.user_pauses(false);
        player.user_closes();

        tokio::time::timeout(Duration::from_secs(5), pump)
            .await
            .expect("the pump ends after the close")
            .expect("no panic");

        let mut to_client = Vec::new();
        while let Some(event) = client.recv().await {
            to_client.push(event);
        }
        let mut to_session = Vec::new();
        while let Ok((_, event)) = heard.try_recv() {
            to_session.push(event);
        }

        assert_eq!(to_client.len(), 3, "{to_client:?}");
        assert_eq!(to_client, to_session);
        assert_eq!(
            to_client.last(),
            Some(&PlayerEvent::Closed(CloseReason::UserQuit))
        );
        assert!(tap.is_closed());
    }

    #[tokio::test]
    async fn a_stream_that_ends_without_a_word_is_reported_as_the_player_closing() {
        let (session, mut heard) = mpsc::unbounded_channel();
        let (player, source) = FakePlayer::new();
        let (sender, ended) = mpsc::unbounded_channel::<PlayerEvent>();
        drop(sender);
        drop(source);
        let (tap, mut client) = Tap::new(player, ended, 1, session);

        pump(Arc::clone(&tap)).await;

        assert_eq!(client.recv().await, None, "the client's stream ends too");
        assert!(matches!(
            heard.try_recv(),
            Ok((1, PlayerEvent::Closed(CloseReason::Crashed(None))))
        ));
    }
}
