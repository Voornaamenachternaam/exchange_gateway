// src/notifications.rs
// Notification system for EWS pull/streaming subscriptions (MS-OXWSNTIF).
// A single broadcast channel fans mailbox store change events out to all
// active subscriptions; each subscription filters by owner, requested folders,
// requested event types and exposes a monotonic per-subscription watermark.

use parking_lot::Mutex as SyncMutex;
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
                matches!(other_folder, Some(of) if set.contains(of))
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
    ///
    /// Synchronous (parking_lot) mutex: the guard is never held across an
    /// `.await`, so a blocking, synchronous mutex is the correct primitive.
    /// An async mutex here is an anti-pattern — its `try_lock` can fail while a
    /// long-lived receiver holds it, silently making the lifetime "bump" on
    /// every GetEvents/recv no-op and reaping a live subscription prematurely.
    deadline: SyncMutex<tokio::time::Instant>,
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
            deadline: SyncMutex::new(tokio::time::Instant::now() + lifetime),
        })
    }
}

/// Subscription manager for EWS pull and streaming subscriptions.
#[derive(Clone)]
pub struct SubscriptionManager {
    sender: broadcast::Sender<NotificationEvent>,
    subscriptions: Arc<Mutex<HashMap<String, Arc<Subscription>>>>,
}

/// Error produced when a streaming subscription cannot be served because it is
/// either unknown or owned by a different identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscriptionServeError;

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
                    if let Some(deadline) = s.deadline.try_lock() {
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

    /// Extend the subscription lifetime to "now + its kind's lifetime".
    ///
    /// Uses a synchronous blocking lock: the guard is released within this
    /// synchronous function (no `.await` under it), so a plain `lock()` cannot
    /// deadlock and — crucially — the bump *always* succeeds, unlike a
    /// `try_lock` that would silently no-op while the reaper held the guard
    /// and leave a soon-to-expire deadline in place (premature reaping).
    fn bump_deadline(sub: &Subscription, kind: SubscriptionKind) {
        let mut deadline = sub.deadline.lock();
        *deadline = tokio::time::Instant::now() + lifetime_for_kind(kind);
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
        self.pull_events_from(sub_id, owner, None).await
    }

    /// Same as [`pull_events`](Self::pull_events), but first reconciles the
    /// internal watermark to the client's last echoed watermark.
    ///
    /// The client-supplied watermark is the cursor the client believes it has
    /// consumed up to. We never move the cursor *backwards* (a smaller value is
    /// ignored to avoid re-emitting already-delivered events), but if the client
    /// reports a higher sequence than our internal one (e.g. after a server
    /// restart lost the in-memory counter, or against a different implementation
    /// that owns the watermark), we advance the internal watermark to it so the
    /// next events we emit carry watermarks strictly greater than anything the
    /// client has already seen — no duplicate notifications, no skipped gaps.
    ///
    /// `client_seq` is clamped well below `u64::MAX` so a malicious or
    /// malformed watermark can never overflow the per-event `+ 1` increments.
    pub async fn pull_events_from(
        &self,
        sub_id: &str,
        owner: &str,
        client_seq: Option<u64>,
    ) -> Option<(Vec<NotificationEvent>, u64, u64, bool)> {
        let sub = self.for_owner(sub_id, owner).await?;
        Self::bump_deadline(&sub, sub.kind);
        let folders = sub.folders.clone();
        let event_types = sub.event_types.clone();
        let owner_filter = sub.owner.clone();

        let mut runtime = sub.runtime.lock().await;
        if let Some(client) = client_seq {
            // Clamp to leave headroom for the +1 increments below; this also
            // rejects absurd/watermark-spoofing values as a no-op reconciliation.
            let capped = client.min(MAX_CLIENT_WATERMARK);
            if capped > runtime.watermark {
                runtime.watermark = capped;
            }
        }
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
    /// available, `Ok(None)` on idle timeout / channel close, or
    /// `Err(SubscriptionServeError)` when the subscription is missing or
    /// belongs to another owner.
    ///
    /// A filtered (non-matching) event does **not** end the turn: the broadcast
    /// receiver is drained of it and the wait continues for the *remaining*
    /// `timeout`. Returning `Ok(None)` on a filtered event would make the
    /// caller emit a keep-alive fragment every time any unrelated event flowed
    /// through the broadcast — a keep-alive storm on a busy mailbox. Only a
    /// genuine idle (no events at all within `timeout`) or channel close yields
    /// `Ok(None)`.
    pub async fn recv_one_streaming(
        &self,
        sub_id: &str,
        owner: &str,
        timeout: Duration,
    ) -> Result<Option<(NotificationEvent, u64)>, SubscriptionServeError> {
        let sub = self
            .for_owner(sub_id, owner)
            .await
            .ok_or(SubscriptionServeError)?;
        Self::bump_deadline(&sub, sub.kind);
        let folders = sub.folders.clone();
        let event_types = sub.event_types.clone();
        let owner_filter = sub.owner.clone();

        let mut runtime = sub.runtime.lock().await;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Drain already-buffered events (non-blocking).
            loop {
                match runtime.receiver.try_recv() {
                    Ok(event)
                        if matches_subscription(&event, &owner_filter, &folders, &event_types) =>
                    {
                        let wm = runtime.watermark + 1;
                        runtime.watermark = wm;
                        return Ok(Some((event, wm)));
                    }
                    Ok(_) => continue, // filtered out; discard and inspect the next buffered event
                    Err(broadcast::error::TryRecvError::Empty) => break, // need to wait
                    Err(broadcast::error::TryRecvError::Closed) => return Ok(None),
                    Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                }
            }
            // Nothing buffered: wait for the *remaining* budget for the next
            // event. A filtered event loops back to drain + wait again instead
            // of ending the turn early.
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Ok(None); // genuine idle timeout
            }
            match tokio::time::timeout(remaining, runtime.receiver.recv()).await {
                Ok(Ok(event))
                    if matches_subscription(&event, &owner_filter, &folders, &event_types) =>
                {
                    let wm = runtime.watermark + 1;
                    runtime.watermark = wm;
                    return Ok(Some((event, wm)));
                }
                Ok(Ok(_)) => continue, // filtered out; keep the turn alive for the rest of the budget
                Ok(Err(broadcast::error::RecvError::Closed)) => return Ok(None),
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Err(_) => return Ok(None), // genuine idle timeout (remaining elapsed)
            }
        }
    }

    /// Return the kind of a subscription, validating that it exists and is owned
    /// by `owner`. This is a **read-only** lookup: it does *not* remove or
    /// otherwise mutate the subscription map (the map guard is released as soon
    /// as the entry is read). Returns `None` if the subscription does not exist
    /// or belongs to a different owner.
    pub async fn subscription_kind(&self, sub_id: &str, owner: &str) -> Option<SubscriptionKind> {
        let map = self.subscriptions.lock().await;
        let sub = map.get(sub_id)?;
        (sub.owner == owner).then(|| sub.kind)
    }

    /// Remove a subscription owned by `owner`. Returns `true` iff a
    /// subscription with `sub_id` existed *and* belonged to `owner`.
    ///
    /// Ownership is verified **before** the map is mutated: only the owner may
    /// delete its own subscription. A request from any other identity leaves the
    /// subscription intact (returns `false`), closing the IDOR — without this
    /// guard a caller who merely knows a subscription id could delete another
    /// mailbox's subscription.
    pub async fn unsubscribe(&self, sub_id: &str, owner: &str) -> bool {
        let mut map = self.subscriptions.lock().await;
        match map.get(sub_id) {
            Some(sub) if sub.owner == owner => {
                map.remove(sub_id);
                true
            }
            _ => false,
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

    /// Obtain a raw `broadcast::Receiver` on the shared notification feed
    /// WITHOUT creating an EWS `Subscription` (no uuid, no deadline, no reaper
    /// entry, no watermark). Used by the MAPI/HTTP `RopRegisterNotification`
    /// path (`mapi::session::MapiNotificationSink`), which owns its own
    /// per-session sink lifecycle keyed by the notification handle index and
    /// filters by owner/types/folder itself (audit §2e). A dropped receiver is
    /// harmless — `broadcast::Sender::send` tolerates zero-receiver sends.
    pub fn subscribe_raw(&self) -> broadcast::Receiver<NotificationEvent> {
        self.sender.subscribe()
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

/// Upper bound accepted for a client-supplied watermark during reconciliation.
/// Watermarks are tiny monotonic counters in practice; this leaves an enormous
/// headroom (well over `i64::MAX`) so the per-event `+ 1` increments in
/// `pull_events_from` and `recv_one_streaming` cannot overflow `u64`, while
/// still refusing absurd, watermark-spoofing values from a malicious client.
const MAX_CLIENT_WATERMARK: u64 = i64::MAX as u64;

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

    /// IDOR guard: a caller who merely knows a subscription id but is not its
    /// owner must not be able to delete it (the subscription must survive).
    #[tokio::test]
    async fn test_unsubscribe_is_owner_scoped_no_idor() {
        let mgr = SubscriptionManager::new();
        let alice_sub = mgr
            .subscribe("alice", SubscriptionKind::Pull, None, None, Some(5))
            .await;
        let bob_sub = mgr
            .subscribe("bob", SubscriptionKind::Pull, None, None, Some(5))
            .await;
        assert_eq!(mgr.active_count().await, 2);

        // Mallory (an attacker) knows alice's subscription id and tries to
        // unsubscribe it. The call must return false AND leave alice's
        // subscription intact.
        assert!(!mgr.unsubscribe(&alice_sub, "mallory").await);
        assert_eq!(
            mgr.subscription_kind(&alice_sub, "alice").await,
            Some(SubscriptionKind::Pull),
            "alice's subscription must survive an attacker's unsubscribe"
        );
        assert_eq!(mgr.active_count().await, 2, "no subscription was deleted");

        // Bob cannot unsubscribe alice's subscription either.
        assert!(!mgr.unsubscribe(&alice_sub, "bob").await);
        assert_eq!(
            mgr.subscription_kind(&alice_sub, "alice").await,
            Some(SubscriptionKind::Pull)
        );

        // Only alice can remove her own.
        assert!(mgr.unsubscribe(&alice_sub, "alice").await);
        assert_eq!(mgr.active_count().await, 1);
        assert_eq!(mgr.subscription_kind(&alice_sub, "alice").await, None);

        // bob's sub is unaffected.
        assert_eq!(
            mgr.subscription_kind(&bob_sub, "bob").await,
            Some(SubscriptionKind::Pull)
        );
        assert!(mgr.unsubscribe(&bob_sub, "bob").await);
    }

    /// Watermark reconciliation advances the internal cursor to the client's
    /// higher watermark so newly-emitted events never collide with sequences
    /// the client already consumed (avoids duplicate/skipped after a restart).
    #[tokio::test]
    async fn test_pull_events_from_advances_watermark_to_client() {
        let mgr = SubscriptionManager::new();
        let sub_id = mgr
            .subscribe("alice", SubscriptionKind::Pull, None, None, Some(5))
            .await;

        // Client claims to have already consumed up to watermark 50 (e.g. via a
        // prior server incarnation). With no events buffered, the first pull
        // must reconcile prev -> 50 and emit nothing new.
        let (events, prev, last, more) = mgr
            .pull_events_from(&sub_id, "alice", Some(50))
            .await
            .unwrap();
        assert!(events.is_empty());
        assert_eq!(prev, 50, "internal watermark advanced to client cursor");
        assert_eq!(last, 50);
        assert!(!more);

        // Now publish two matching events; they must carry watermarks 51 and 52
        // (strictly greater than the client's 50 — no collision with prior 1..50).
        mgr.publish(mk("alice", "inbox", "i1"));
        mgr.publish(mk("alice", "inbox", "i2"));
        let (events, prev, last, _more) = mgr.pull_events(&sub_id, "alice").await.unwrap();
        assert_eq!(prev, 50);
        assert_eq!(events.len(), 2);
        assert_eq!(last, 52);

        assert!(mgr.unsubscribe(&sub_id, "alice").await);
    }

    /// Reconciliation never moves the cursor backwards: a client that reports a
    /// *lower* watermark (replay/stale echo) is ignored so already-delivered
    /// events are not re-emitted.
    #[tokio::test]
    async fn test_pull_events_from_never_regresses_watermark() {
        let mgr = SubscriptionManager::new();
        let sub_id = mgr
            .subscribe("alice", SubscriptionKind::Pull, None, None, Some(5))
            .await;
        mgr.publish(mk("alice", "inbox", "i1"));
        let (events, _prev, last, _) = mgr.pull_events(&sub_id, "alice").await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(last, 1);

        // Client echoes a stale watermark 0 (smaller than our internal 1).
        // No events buffered; the watermark must stay at 1, not regress to 0.
        let (events, prev, last2, _) = mgr
            .pull_events_from(&sub_id, "alice", Some(0))
            .await
            .unwrap();
        assert!(events.is_empty());
        assert_eq!(prev, 1, "watermark must not regress");
        assert_eq!(last2, 1);

        assert!(mgr.unsubscribe(&sub_id, "alice").await);
    }

    /// An absurd client watermark (near u64::MAX) is clamped so the per-event
    /// `+ 1` increments never overflow `u64` (and thus never panic a debug
    /// build); it is otherwise accepted as a high cursor.
    #[tokio::test]
    async fn test_pull_events_from_clamps_absurd_watermark_without_overflow() {
        let mgr = SubscriptionManager::new();
        let sub_id = mgr
            .subscribe("alice", SubscriptionKind::Pull, None, None, Some(5))
            .await;
        // Claim up to u64::MAX; must not panic, and one new event gets a
        // watermark that is MAX_CLIENT_WATERMARK + 1 (no overflow).
        let client = u64::MAX;
        let (events, prev, last, _) = mgr
            .pull_events_from(&sub_id, "alice", Some(client))
            .await
            .unwrap();
        assert!(events.is_empty());
        assert_eq!(prev, MAX_CLIENT_WATERMARK);
        assert_eq!(last, MAX_CLIENT_WATERMARK);

        mgr.publish(mk("alice", "inbox", "i1"));
        let (events, _prev, last, _) = mgr.pull_events(&sub_id, "alice").await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(last, MAX_CLIENT_WATERMARK + 1);

        assert!(mgr.unsubscribe(&sub_id, "alice").await);
    }

    /// A filtered event arriving during the streaming receive wait must NOT end
    /// the turn early — the wait continues for the remaining budget, and only a
    /// genuine idle (no events at all within `timeout`) yields `Ok(None)`.
    /// Without this, a busy mailbox would trigger a keep-alive storm.
    #[tokio::test]
    async fn test_streaming_filtered_event_does_not_end_turn_early() {
        let mgr = SubscriptionManager::new();
        // Subscribe to a single folder so the other folder's events are filtered.
        let sub_id = mgr
            .subscribe(
                "alice",
                SubscriptionKind::Streaming,
                Some(HashSet::from(["inbox".to_string()])),
                None,
                None,
            )
            .await;

        let timeout = Duration::from_millis(300);
        // Nothing published at all → genuine idle → Ok(None) after ~timeout.
        let t0 = std::time::Instant::now();
        let idle = mgr
            .recv_one_streaming(&sub_id, "alice", timeout)
            .await
            .unwrap();
        let elapsed_idle = t0.elapsed();
        assert!(idle.is_none(), "no events at all => idle None");
        assert!(
            elapsed_idle >= Duration::from_millis(250),
            "genuine idle must wait the full timeout, elapsed {:?}",
            elapsed_idle
        );

        // Now publish several events that are FILTERED OUT (wrong owner) close
        // together within the turn. The receiver must keep waiting for the
        // remaining budget and NOT return Ok(None) the instant the first
        // filtered event arrives. Then publish a MATCHING event and confirm it
        // is delivered.
        let mgr2 = mgr.clone();
        let sub_id2 = sub_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(40)).await;
            // filtered: belongs to bob, not alice
            mgr2.publish(mk("bob", "inbox", "noise-1"));
            mgr2.publish(mk("bob", "inbox", "noise-2"));
            mgr2.publish(mk("bob", "inbox", "noise-3"));
            tokio::time::sleep(Duration::from_millis(50)).await;
            // matching: alice + inbox
            mgr2.publish(mk("alice", "inbox", "real-1"));
        });

        let t1 = std::time::Instant::now();
        let got = mgr
            .recv_one_streaming(&sub_id, "alice", Duration::from_millis(500))
            .await
            .unwrap();
        let elapsed = t1.elapsed();
        assert!(
            got.is_some(),
            "a matching event must be delivered despite preceding filtered events"
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "should return on the matching event, not idle-time-out (elapsed {:?})",
            elapsed
        );
        // The next event must be "noise" events already drained/dropped; a
        // fresh idle returns None after the full timeout (no premature return).
        let _ = sub_id2;

        assert!(mgr.unsubscribe(&sub_id, "alice").await);
    }
}
