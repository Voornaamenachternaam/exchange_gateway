// src/notifications.rs
// Notification system for EWS streaming subscriptions and EAS Ping coordination.
// Uses a broadcast channel to deliver change events to waiting subscribers.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use tokio::sync::{broadcast, Mutex};

/// Notification event types.
#[derive(Debug, Clone)]
pub enum NotificationEvent {
    /// A new item was created in a folder.
    ItemCreated { folder_id: String, item_id: String },
    /// An item was modified in a folder.
    ItemModified { folder_id: String, item_id: String },
    /// An item was deleted from a folder.
    ItemDeleted { folder_id: String, item_id: String },
    /// A status change occurred (e.g., read flag, categories).
    ItemStatusChanged { folder_id: String, item_id: String },
    /// A new email arrived (specific for email).
    NewMail { mailbox: String },
}

/// Subscription manager for long-lived EWS Subscribe connections.
#[derive(Clone)]
pub struct SubscriptionManager {
    /// Broadcast channel for notifications.
    sender: broadcast::Sender<NotificationEvent>,
    /// Active subscriptions: SubscriptionId -> receiver handle.
    subscriptions: Arc<Mutex<HashMap<String, broadcast::Receiver<NotificationEvent>>>>,
}

impl SubscriptionManager {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(1024);
        Self {
            sender,
            subscriptions: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    
    /// Create a new subscription and return the subscription ID.
    /// The caller can then await on the receiver returned by get_receiver().
    pub async fn subscribe(&self) -> String {
        let mut subs = self.subscriptions.lock().await;
        let sub_id = uuid::Uuid::new_v4().to_string();
        let receiver = self.sender.subscribe();
        subs.insert(sub_id.clone(), receiver);
        sub_id
    }
    
    /// Get a receiver for an existing subscription ID.
    /// Returns None if the subscription does not exist.
    pub async fn get_receiver(&self, sub_id: &str) -> Option<broadcast::Receiver<NotificationEvent>> {
        let mut subs = self.subscriptions.lock().await;
        subs.remove(sub_id)
    }
    
    /// Unsubscribe (remove) a subscription.
    pub async fn unsubscribe(&self, sub_id: &str) -> bool {
        let mut subs = self.subscriptions.lock().await;
        subs.remove(sub_id).is_some()
    }
    
    /// Send a notification event to all active subscribers.
    /// Errors (e.g., no receivers) are ignored – closed subscriptions will be cleaned lazily.
    pub fn send(&self, event: NotificationEvent) {
        let _ = self.sender.send(event);
    }
    
    /// Return the broadcast sender for direct use (e.g., external events feeding in).
    pub fn sender(&self) -> broadcast::Sender<NotificationEvent> {
        self.sender.clone()
    }
}

impl Default for SubscriptionManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Global notification manager instance.
/// In a real deployment, this would be stored in AppState.
static GLOBAL_SUBSCRIPTION_MANAGER: LazyLock<SubscriptionManager> = LazyLock::new(SubscriptionManager::new);

/// Get the global subscription manager, initializing if necessary.
pub fn global_manager() -> &'static SubscriptionManager {
    &GLOBAL_SUBSCRIPTION_MANAGER
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_subscription_create_and_send() {
        let mgr = SubscriptionManager::new();
        let sub_id = mgr.subscribe().await;
        assert!(mgr.get_receiver(&sub_id).await.is_some());
        mgr.send(NotificationEvent::NewMail { mailbox: "inbox".to_string() });
        // In a real test, we would await on the receiver.
        assert!(mgr.unsubscribe(&sub_id).await);
    }
    
    #[tokio::test]
    async fn test_async_subscription() {
        let mgr = SubscriptionManager::new();
        let sub_id = mgr.subscribe().await;
        let mut receiver = mgr.get_receiver(&sub_id).await.unwrap();
        
        mgr.send(NotificationEvent::ItemCreated {
            folder_id: "folder1".to_string(),
            item_id: "item1".to_string(),
        });
        
        // Use try_recv to avoid blocking the test
        match receiver.try_recv() {
            Ok(event) => {
                match event {
                    NotificationEvent::ItemCreated { folder_id, item_id } => {
                        assert_eq!(folder_id, "folder1");
                        assert_eq!(item_id, "item1");
                    }
                    _ => panic!("Unexpected event type"),
                }
            }
            Err(_) => panic!("No event received"),
        }
        
        mgr.unsubscribe(&sub_id).await;
    }
}