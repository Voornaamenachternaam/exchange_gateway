// src/notifications.rs
// Notification system for EWS pull/streaming subscriptions (MS-OXWSNTIF).
// A single broadcast channel fans mailbox store change events out to all
// active subscriptions; each subscription filters by owner, requested folders,
// requested event types and exposes a monotonic per-subscription watermark.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, broadcast};

/// Maximum lifetime of a pull (GetEvents) subscription, in minutes, matching
/// the `SubscriptionTimeoutType` upper bound (MS-OXWSNTIF 3.1.4.3.4.2).
const PULL_SUBSCRIBER_MAX_MINUTES: u64 = 1440;
/// Default pull subscription idle timeout when the client omits `<Timeout>`.
const PULL_SUBSCRIBER_DEFAULT_MINUTES: u64 = 30;
/// Sweep interval for expired subscriptions.
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// The class of notification event the backend store raised.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NotificationEvent {
    /// A new item was created in a folder.
    ItemCreated {
        owner: String,
        folder_id: String,
        item_id: String,
        change_key: String,
    },
    /// An item was modified in a folder.
    ItemModified {
        owner: String,
        folder_id: String,
        item_id: String,
        change_key: String,
    },
    /// An item was deleted from a folder.
    ItemDeleted {
        owner: String,
        folder_id: String,
        item_id: String,
    },
    /// A new email arrived.
    NewMail {
        owner: String,
        folder_id: String,
        item_id: String,
        change_key: String,
    },
    /// An item was moved between folders.
    ItemMoved {
        owner: String,
        old_folder_id: String,
        old_item_id: String,
        new_folder_id: String,
        new_item_id: String,
        change_key: String,
    },
    /// An item was copied to another folder.
    ItemCopied {
        owner: String,
        old_folder_id: String,
        old_item_id: String,
        new_folder_id: String,
        new_item_id: String,
        change_key: String,
    },
}

impl NotificationEvent {
    /// Canonical EWS event-type name (MS-OXWSNTIF 3.1.4.3.4.1).
    pub fn event_type_name(&self) -> &'static str {
        match self {
            NotificationEvent::ItemCreated { .. } => "CreatedEvent",
            NotificationEvent::ItemModified { .. } => "ModifiedEvent",
            NotificationEvent::ItemDeleted { .. } => "DeletedEvent",
            NotificationEvent::NewMail { .. } => "NewMailEvent",
            NotificationEvent::ItemMoved { .. } => "MovedEvent",
            NotificationEvent::ItemCopied { .. } => "CopiedEvent",
        }
    }

    /// The owning mailbox of the affected item (used for authorization/filtering).
    pub fn owner(&self) -> &str {
        match self {
            NotificationEvent::ItemCreated { owner, .. }
            | NotificationEvent::ItemModified { owner, .. }
            | NotificationEvent::ItemDeleted { owner, .. }
            | NotificationEvent::NewMail { owner, .. }
            | NotificationEvent::ItemMoved { owner, .. }
            | NotificationEvent::ItemCopied { owner, .. } => owner,
        }
    }

    /// Whether this event should be emitted to a subscription filtered to the
    /// given set of folders (`None` => subscribe to all folders).
    fn matches_folders(&self, folders: &Option<HashSet<String>>) -> bool {
        match folders {
            None => true,
            Some(set) => {
                let (this_folder, other_folder) = match self {
                    NotificationEvent::ItemCreated { folder_id, .. }
                    | NotificationEvent::ItemModified { folder_id, .. }
                    | NotificationEvent::ItemDeleted { folder_id, .. }
                    | NotificationEvent::NewMail { folder_id, .. } => (folder_id.as_str(), None),
                    NotificationEvent::ItemMoved {
                        new_folder_id,
                        old_folder_id,
                        ..
                    }
                    | NotificationEvent::ItemCopied {
                        new_folder_id,
                        old_folder_id,
                        ..
                    } => (new_folder_id.as_str(), Some(old_folder_id.as_str())),
                };
                if set.contains(this_folder) {
                    return true;
                }
                match other_folder {
                    Some(of) if set.contains(of) => true,
                    _ => false,
                }
            }
        }
    }

    fn matches_types(&self, types: &Option<HashSet<String>>) -> bool {
        match types {
            None => true,
            Some(set) => set.contains(self.event_type_name()),
        }
    }
}

/// Kind of EWS subscription (MS-OXWSNTIF 3.1.4.3.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionKind {
    Pull,
    Streaming,
}

/// In-memory state for a single subscription.
///
/// Config (`owner`/`kind`/`folders`/`event_types`) is immutable after creation
/// and is read cheaply under the global map lock. Runtime state (the broadcast
/// receiver plus the monotonic watermark) lives behind a per-subscription mutex
/// so that a long-lived GetStreamingEvents `recv` blocks only its own
/// subscription and never the global table.
struct Subscription {
    owner: String,
    kind: SubscriptionKind,
    /// `None` means subscribe to all folders (SubscribeToAllFolders).
    folders: Option<HashSet<String>>,
    /// `None` means subscribe to all event types.
    event_types: Option<HashSet<String>>,
    /// Persistent broadcast receiver and per-subscription watermark.
    runtime: Mutex<SubscriptionRuntime>,
    /// Absolute deadline after which the subscription is reaped.
    deadline: Mutex<tokio::time::Instant>,
}

struct SubscriptionRuntime {
    receiver: broadcast::Receiver<NotificationEvent>,
    watermark: u64,
}

impl Subscription {
    fn new(
        owner: String,
        kind: SubscriptionKind,
        folders: Option<HashSet<String>>,
        event_types: Option<HashSet<String>>,
        receiver: broadcast::Receiver<NotificationEvent>,
        lifetime: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            owner,
            kind,
            folders,
            event_types,
            runtime: Mutex::new(SubscriptionRuntime {
                receiver,
                watermark: 0,
            }),
            deadline: Mutex::new(tokio::time::Instant::now() + lifetime),
        })
    }
}

/// Subscription manager for EWS pull and streaming subscriptions.
#[derive(Clone)]
pub struct SubscriptionManager {
    sender: broadcast::Sender<NotificationEvent>,
    subscriptions: Arc<Mutex<HashMap<String, Arc<Subscription>>>>,
}

impl SubscriptionManager {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(8192);
        let manager = Self {
            sender,
            subscriptions: Arc::new(Mutex::new(HashMap::new())),
        };
        manager.start_reaper();
        manager
    }

    /// Background task that periodically removes expired subscriptions so the
    /// in-memory table cannot grow unbounded if a client never unsubscribes.
    fn start_reaper(&self) {
        let subs = Arc::clone(&self.subscriptions);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
            ticker.tick().await; // consume the immediate first tick
            loop {
                ticker.tick().await;
                let now = tokio::time::Instant::now();
                let mut map = subs.lock().await;
                map.retain(|_, s| {
                    if let Ok(deadline) = s.deadline.try_lock() {
                        *deadline > now
                    } else {
                        // An in-flight operation owns the deadline guard; keep
                        // the entry this sweep, it will be re-checked next pass.
                        true
                    }
                });
            }
        });
    }

    /// Create a subscription and return its id.
    ///
    /// `folders` of `None` subscribes to all folders; an empty set subscribes to
    /// no folders. `event_types` of `None` subscribes to all event types.
    pub async fn subscribe(
        &self,
        owner: &str,
        kind: SubscriptionKind,
        folders: Option<HashSet<String>>,
        event_types: Option<HashSet<String>>,
        timeout_minutes: Option<u32>,
    ) -> String {
        let lifetime = match kind {
            SubscriptionKind::Pull => {
                let mins = timeout_minutes
                    .map(|m| m.clamp(1, PULL_SUBSCRIBER_MAX_MINUTES as u32) as u64)
                    .unwrap_or(PULL_SUBSCRIBER_DEFAULT_MINUTES);
                Duration::from_secs(mins * 60)
            }
            SubscriptionKind::Streaming => {
                // Streaming subscriptions are bounded by the GetStreamingEvents
                // ConnectionTimeout per call; keep a generous idle lifetime.
                Duration::from_secs(PULL_SUBSCRIBER_MAX_MINUTES * 60)
            }
        };
        let receiver = self.sender.subscribe();
        let subscription = Subscription::new(
            owner.to_string(),
            kind,
            folders,
            event_types,
            receiver,
            lifetime,
        );
        let sub_id = uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        let mut map = self.subscriptions.lock().await;
        map.insert(sub_id.clone(), subscription);
        sub_id
    }

    /// Acquire the `Arc<Subscription>` for `sub_id` iff it exists and is owned
    /// by `owner`. Releases the global map lock immediately, so the caller never
    /// holds it across an async wait.
    async fn for_owner(&self, sub_id: &str, owner: &str) -> Option<Arc<Subscription>> {
        let map = self.subscriptions.lock().await;
        let sub = map.get(sub_id)?.clone();
        // Validate ownership under the map lock – it is immutable, so this is
        // a consistent snapshot even after we release the guard.
        (sub.owner == owner).then_some(sub)
    }

    fn bump_deadline(sub: &Subscription, kind: SubscriptionKind) {
        if let Ok(mut deadline) = sub.deadline.try_lock() {
            *deadline = tokio::time::Instant::now() + lifetime_for_kind(kind);
        }
    }

    /// Drain the backlog of events currently buffered for a subscription,
    /// returning the events the subscription is authorized/filtered to receive,
    /// along with the updated watermark to be echoed back to the client.
    ///
    /// Returns `None` if the subscription does not exist or is not owned by
    /// `owner`.
    pub async fn pull_events(
        &self,
        sub_id: &str,
        owner: &str,
    ) -> Option<(Vec<NotificationEvent>, u64, u64, bool)> {
        let sub = self.for_owner(sub_id, owner).await?;
        Self::bump_deadline(&sub, sub.kind);
        let folders = sub.folders.clone();
        let event_types = sub.event_types.clone();
        let owner_filter = sub.owner.clone();

        let mut runtime = sub.runtime.lock().await;
        let prev = runtime.watermark;
        let mut last = prev;
        let mut events = Vec::new();
        let mut more = false;
        loop {
            match runtime.receiver.try_recv() {
                Ok(event) => {
                    if owner_filter == event.owner()
                        && event.matches_folders(&folders)
                        && event.matches_types(&event_types)
                    {
                        last = runtime.watermark + 1;
                        runtime.watermark = last;
                        events.push(event);
                        if events.len() >= MAX_EVENTS_PER_PULL {
                            more = true;
                            break;
                        }
                    }
                }
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Closed) => break,
                Err(broadcast::error::TryRecvError::Lagged(_)) => {
                    // The client is too slow and overflowed the broadcast
                    // buffer. Persist the watermark and stop draining; the next
                    // GetEvents observes a fresh, contiguous stream.
                    tracing::warn!(
                        subscription_id = sub_id,
                        "EWS pull subscription lagged the broadcast buffer; draining"
                    );
                    continue;
                }
            }
        }
        Some((events, prev, last, more))
    }

    /// Try to receive a single matching event for a streaming subscription,
    /// waiting up to `timeout` for one to arrive.
    ///
    /// Returns `Ok(Some((event, watermark)))` when a matching event is
    /// available, `Ok(None)` on idle timeout / channel close, or `Err(())` when
    /// the subscription is missing or belongs to another owner.
    pub async fn recv_one_streaming(
        &self,
        sub_id: &str,
        owner: &str,
        timeout: Duration,
    ) -> Result<Option<(NotificationEvent, u64)>, ()> {
        let sub = self.for_owner(sub_id, owner).await.ok_or(())?;
        Self::bump_deadline(&sub, sub.kind);
        let folders = sub.folders.clone();
        let event_types = sub.event_types.clone();
        let owner_filter = sub.owner.clone();

        let mut runtime = sub.runtime.lock().await;
        loop {
            // Drain already-buffered events (non-blocking).
            match runtime.receiver.try_recv() {
                Ok(event) if matches_subscription(&event, &owner_filter, &folders, &event_types) => {
                    let wm = runtime.watermark + 1;
                    runtime.watermark = wm;
                    return Ok(Some((event, wm)));
                }
                Ok(_) => continue, // filtered out; inspect the next buffered event
                Err(broadcast::error::TryRecvError::Empty) => break, // need to wait
                Err(broadcast::error::TryRecvError::Closed) => return Ok(None),
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
            }
        }
        // Nothing buffered: block up to `timeout` for the next event.
        match tokio::time::timeout(timeout, runtime.receiver.recv()).await {
            Ok(Ok(event)) => {
                if !matches_subscription(&event, &owner_filter, &folders, &event_types) {
                    // Keep-alive / filtered event: not delivered this turn.
                    return Ok(None);
                }
                let wm = runtime.watermark + 1;
                runtime.watermark = wm;
                Ok(Some((event, wm)))
            }
            Ok(Err(broadcast::error::RecvError::Closed)) => Ok(None),
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => Ok(None),
            Err(_) => Ok(None), // idle timeout
        }
    }

    /// Remove and return the kind of a subscription (used to validate
    /// GetEvents vs GetStreamingEvents usage).
    pub async fn subscription_kind(&self, sub_id: &str, owner: &str) -> Option<SubscriptionKind> {
        let map = self.subscriptions.lock().await;
        let sub = map.get(sub_id)?;
        (sub.owner == owner).then(|| sub.kind)
    }

    /// Remove a subscription; returns true if it existed for `owner`.
    pub async fn unsubscribe(&self, sub_id: &str, owner: &str) -> bool {
        let mut map = self.subscriptions.lock().await;
        match map.remove(sub_id) {
            Some(sub) => sub.owner == owner,
            None => false,
        }
    }

    /// Record the current number of active subscriptions (observability/tests).
    pub async fn active_count(&self) -> usize {
        self.subscriptions.lock().await.len()
    }

    /// Broadcast a change event to all subscribers. Errors (no receivers) are
    /// benign and are ignored; stale subscriptions are reaped by the background
    /// task.
    pub fn publish(&self, event: NotificationEvent) {
        let _ = self.sender.send(event);
    }
}

impl Default for SubscriptionManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Maximum number of events returned in a single GetEvents drain so a single
/// busy mailbox cannot produce arbitrarily large responses.
const MAX_EVENTS_PER_PULL: usize = 512;

fn lifetime_for_kind(kind: SubscriptionKind) -> Duration {
    match kind {
        SubscriptionKind::Pull => Duration::from_secs(PULL_SUBSCRIBER_DEFAULT_MINUTES * 60),
        SubscriptionKind::Streaming => Duration::from_secs(PULL_SUBSCRIBER_MAX_MINUTES * 60),
    }
}

fn matches_subscription(
    event: &NotificationEvent,
    owner: &str,
    folders: &Option<HashSet<String>>,
    event_types: &Option<HashSet<String>>,
) -> bool {
    event.owner() == owner && event.matches_folders(folders) && event.matches_types(event_types)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(owner: &str, folder: &str, item: &str) -> NotificationEvent {
        NotificationEvent::ItemCreated {
            owner: owner.to_string(),
            folder_id: folder.to_string(),
            item_id: item.to_string(),
            change_key: "1".to_string(),
        }
    }

    #[tokio::test]
    async fn test_subscribe_pull_and_unsubscribe() {
        let mgr = SubscriptionManager::new();
        let sub_id = mgr
            .subscribe("alice", SubscriptionKind::Pull, None, None, Some(1))
            .await;
        assert_eq!(mgr.active_count().await, 1);
        assert!(mgr.subscription_kind(&sub_id, "alice").await.is_some());
        assert!(mgr.subscription_kind(&sub_id, "mallory").await.is_none());
        assert!(mgr.unsubscribe(&sub_id, "alice").await);
        assert_eq!(mgr.active_count().await, 0);
    }

    #[tokio::test]
    async fn test_pull_drains_filtered_events() {
        let mgr = SubscriptionManager::new();
        let sub_id = mgr
            .subscribe(
                "alice",
                SubscriptionKind::Pull,
                Some(HashSet::from(["inbox".to_string()])),
                Some(HashSet::from(["CreatedEvent".to_string()])),
                Some(1),
            )
            .await;
        mgr.publish(mk("alice", "inbox", "i1"));
        mgr.publish(mk("alice", "calendar", "i2")); // folder filter: dropped
        mgr.publish(NotificationEvent::ItemModified {
            owner: "alice".to_string(),
            folder_id: "inbox".to_string(),
            item_id: "i3".to_string(),
            change_key: "1".to_string(),
        }); // type filter: dropped
        mgr.publish(mk("bob", "inbox", "i4")); // owner filter: dropped

        let (events, prev, last, more) = mgr.pull_events(&sub_id, "alice").await.unwrap();
        assert_eq!(prev, 0);
        assert_eq!(last, 1);
        assert!(!more);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], NotificationEvent::ItemCreated { .. }));

        // Second pull returns nothing new.
        let (events2, prev2, last2, more2) = mgr.pull_events(&sub_id, "alice").await.unwrap();
        assert!(events2.is_empty());
        assert_eq!(prev2, last);
        assert_eq!(last2, last);
        assert!(!more2);

        assert!(mgr.unsubscribe(&sub_id, "alice").await);
    }

    #[tokio::test]
    async fn test_pull_unknown_subscription() {
        let mgr = SubscriptionManager::new();
        assert!(mgr.pull_events("nope", "alice").await.is_none());
    }

    #[tokio::test]
    async fn test_streaming_recv_one() {
        let mgr = SubscriptionManager::new();
        let sub_id = mgr
            .subscribe("alice", SubscriptionKind::Streaming, None, None, None)
            .await;
        mgr.publish(mk("alice", "inbox", "i1"));
        let got = mgr
            .recv_one_streaming(&sub_id, "alice", Duration::from_millis(50))
            .await
            .unwrap();
        assert!(got.is_some());
        let idle = mgr
            .recv_one_streaming(&sub_id, "alice", Duration::from_millis(50))
            .await
            .unwrap();
        assert!(idle.is_none());
        assert!(mgr.unsubscribe(&sub_id, "alice").await);
    }

    #[tokio::test]
    async fn test_publish_to_no_subscribers_is_safe() {
        let mgr = SubscriptionManager::new();
        mgr.publish(mk("alice", "inbox", "i1"));
        assert_eq!(mgr.active_count().await, 0);
    }
}
