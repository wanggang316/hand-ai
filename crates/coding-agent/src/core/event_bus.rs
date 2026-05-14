//! Typed broadcast event bus.
//!
//! Wraps [`tokio::sync::broadcast`] in a typed channel. Each
//! [`EventBus`] instance is parametrised over a single event type `E`
//! — different channels become different bus instances or different
//! enum variants, which keeps subscriber types explicit and avoids the
//! `Box<dyn Fn>` callback pattern.

use tokio::sync::broadcast;

/// Default subscriber buffer per bus.
///
/// Lagging subscribers older than this drop the oldest events; that mirrors
/// the lossy semantics of `EventEmitter` listeners that fall behind.
pub const DEFAULT_BUS_CAPACITY: usize = 64;

/// Typed broadcast bus.
///
/// Cheap to clone — the underlying [`broadcast::Sender`] is a reference-counted
/// handle, so multiple producers can share the same bus by cloning it.
#[derive(Debug)]
pub struct EventBus<E>
where
    E: Clone + Send + 'static,
{
    sender: broadcast::Sender<E>,
}

impl<E> EventBus<E>
where
    E: Clone + Send + 'static,
{
    /// Create a new bus with [`DEFAULT_BUS_CAPACITY`].
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_BUS_CAPACITY)
    }

    /// Create a new bus with a custom subscriber buffer.
    ///
    /// The capacity must be at least 1; values smaller than that are clamped
    /// to 1 to avoid panics inside [`broadcast::channel`].
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let (sender, _initial_rx) = broadcast::channel(capacity);
        Self { sender }
    }

    /// Subscribe to future events. Each receiver sees every event published
    /// after the subscription is created.
    pub fn subscribe(&self) -> broadcast::Receiver<E> {
        self.sender.subscribe()
    }

    /// Publish an event to all current subscribers.
    ///
    /// Returns the number of subscribers that received the event. If there are
    /// no subscribers, the event is dropped and `0` is returned. This mirrors
    /// the fire-and-forget semantics of the TS reference, where emitting on a
    /// channel with no listeners is a no-op.
    pub fn publish(&self, event: E) -> usize {
        // `broadcast::Sender::send` errors only when there are no receivers,
        // which we treat as a successful no-op.
        self.sender.send(event).unwrap_or(0)
    }

    /// Number of currently active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl<E> Default for EventBus<E>
where
    E: Clone + Send + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<E> Clone for EventBus<E>
where
    E: Clone + Send + 'static,
{
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast::error::TryRecvError;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum TestEvent {
        Ping,
        Pong(u32),
    }

    #[tokio::test]
    async fn subscribe_receives_published_events() {
        let bus: EventBus<TestEvent> = EventBus::new();
        let mut rx = bus.subscribe();

        bus.publish(TestEvent::Ping);
        bus.publish(TestEvent::Pong(7));

        assert_eq!(rx.recv().await.expect("recv ping"), TestEvent::Ping);
        assert_eq!(rx.recv().await.expect("recv pong"), TestEvent::Pong(7));
    }

    #[tokio::test]
    async fn publish_without_subscribers_is_a_noop() {
        let bus: EventBus<TestEvent> = EventBus::new();
        // No subscribers — should return 0 and not panic.
        assert_eq!(bus.publish(TestEvent::Ping), 0);
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn multiple_subscribers_all_observe_event() {
        let bus: EventBus<TestEvent> = EventBus::new();
        let mut rx_a = bus.subscribe();
        let mut rx_b = bus.subscribe();

        let delivered = bus.publish(TestEvent::Pong(1));
        assert_eq!(delivered, 2);
        assert_eq!(bus.subscriber_count(), 2);
        assert_eq!(rx_a.recv().await.expect("rx_a"), TestEvent::Pong(1));
        assert_eq!(rx_b.recv().await.expect("rx_b"), TestEvent::Pong(1));
    }

    #[tokio::test]
    async fn subscribers_only_see_events_after_subscription() {
        let bus: EventBus<TestEvent> = EventBus::new();
        bus.publish(TestEvent::Ping); // no subscribers, dropped

        let mut rx = bus.subscribe();
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));

        bus.publish(TestEvent::Pong(2));
        assert_eq!(
            rx.recv().await.expect("post-subscribe event"),
            TestEvent::Pong(2)
        );
    }

    #[tokio::test]
    async fn cloned_bus_shares_subscribers() {
        let bus: EventBus<TestEvent> = EventBus::new();
        let mut rx = bus.subscribe();

        let cloned = bus.clone();
        cloned.publish(TestEvent::Ping);

        assert_eq!(
            rx.recv().await.expect("recv via cloned bus"),
            TestEvent::Ping
        );
    }

    #[tokio::test]
    async fn capacity_is_clamped_to_minimum_one() {
        // Should not panic even though we requested 0.
        let bus: EventBus<TestEvent> = EventBus::with_capacity(0);
        let mut rx = bus.subscribe();
        bus.publish(TestEvent::Ping);
        assert_eq!(rx.recv().await.expect("recv"), TestEvent::Ping);
    }
}
