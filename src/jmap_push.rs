// src/jmap_push.rs
//
// Live JMAP push subscription (`urn:ietf:params:jmap:push`, RFC 8620 §7.2).
//
// The gateway's email-change detection historically relied on *client-driven*
// polling: a mailbox only learned about new mail when a client issued an
// `Email/changes`-backed Sync / Ping. That leaves the MAPI `NotificationWait`
// / `RopNotify` path (New Outlook) and EAS `Ping` dependent on the client's
// poll cadence, so "new mail" can lag by the poll interval (audit gap #11).
//
// This module replaces that reliance with a live `text/event-stream` (SSE)
// subscription to Stalwart's JMAP push endpoint. The server emits a JMAP
// `StateChange` event (RFC 8620 §7.2.1) whenever the account's `Email` state
// advances; on each event the monitor resolves the delta through the existing
// `Email/changes` + `Email/get` path (already used by EWS/EAS sync) and
// publishes `NotificationEvent::NewMail` into the shared subscription manager.
// The credentials are the user's own (Basic auth), obtained in-band at the
// point the client registers for notifications — never persisted.
//
// SSE wire format (WHATWG HTML "server-sent events"):
//   event: Email
//   data: {"@type":"StateChange","changed":{"<accountId>":{"Email":"<state>"}}}
//
// A frame is terminated by a blank line; `data:` lines within a frame are
// joined with a single `\n`. Reconnects are handled with bounded exponential
// backoff, and the monitor self-terminates when its shutdown token is
// signalled (e.g. the last MAPI notification sink for the mailbox is released).

use crate::jmap::JmapClient;
use crate::notifications::{NotificationEvent, SubscriptionManager};
use reqwest::header::AUTHORIZATION;
use secrecy::SecretString;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// One parsed server-sent event frame (event type and its `data:` payload).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: Option<String>,
}

/// Parse a raw `text/event-stream` chunk into zero or more complete frames.
///
/// Frames are separated by a blank line (`\n\n`). Within a frame, `event:` and
/// `data:` fields are captured; multiple `data:` lines are joined with `\n`
/// (per the WHATWG incrementing rules). Comment lines (`:` prefix) and
/// `retry:`/`id:` fields are ignored. Partial frames (no trailing blank line)
/// are returned in `rest` so the caller can prepend it to the next chunk.
pub fn parse_sse_frames(input: &str) -> (Vec<SseEvent>, String) {
    let mut frames = Vec::new();

    // Normalise CRLF / lone-CR line endings to `\n` so the blank-line frame
    // separator can be located uniformly (SSE allows any of `\r\n`, `\n`, `\r`).
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");

    // Split into complete frames on blank-line boundaries, then parse each
    // frame's fields. The trailing segment after the final blank line is a
    // partial frame and is returned in `rest` for the caller to re-feed with
    // the next chunk.
    let mut last_blank = None;
    let mut idx = 0usize;
    while let Some(pos) = normalized[idx..].find("\n\n") {
        last_blank = Some(idx + pos);
        idx += pos + 2;
    }

    let complete_end = last_blank.map(|p| p + 2).unwrap_or(0);
    let complete = &normalized[..complete_end];
    let rest = normalized[complete_end..].to_string();

    for frame_text in complete.split("\n\n") {
        let frame_text = frame_text.trim_matches('\n');
        if frame_text.is_empty() {
            continue;
        }
        let mut event: Option<String> = None;
        let mut data_lines: Vec<String> = Vec::new();
        for line in frame_text.lines() {
            if let Some(comment) = line.strip_prefix(':') {
                let _ = comment;
                continue;
            }
            match line.split_once(':') {
                Some(("event", value)) => event = Some(value.trim().to_string()),
                Some(("data", value)) => {
                    data_lines.push(value.strip_prefix(' ').unwrap_or(value).to_string())
                }
                _ => { /* retry/id — ignored */ }
            }
        }
        let data = if data_lines.is_empty() {
            None
        } else {
            Some(data_lines.join("\n"))
        };
        if event.is_some() || data.is_some() {
            frames.push(SseEvent { event, data });
        }
    }

    (frames, rest)
}

/// A parsed JMAP `StateChange` push payload (RFC 8620 §7.2.1): the data type(s)
/// whose state advanced and the new state token for each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateChange {
    pub account_id: String,
    /// (data_type, new_state) pairs — e.g. `("Email", "s-123")`.
    pub changed: Vec<(String, String)>,
}

/// Parse a JMAP `StateChange` JSON `data` payload into `StateChange`.
///
/// The payload is `{ "changed": { "<accountId>": { "<type>": "<state>" } } }`.
/// Handles the standard single-account shape; multiple accounts are flattened
/// into the returned `changed` vec. Returns `None` for malformed / non-
/// `StateChange` payloads.
pub fn parse_state_change(data: &str) -> Option<StateChange> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    let changed = value.get("changed")?.as_object()?;
    // Take the first account (the gateway's account is the only one).
    let (account_id, types) = changed.iter().next()?;
    let mut pairs = Vec::new();
    for (data_type, state) in types.as_object()? {
        pairs.push((data_type.clone(), state.as_str()?.to_string()));
    }
    Some(StateChange {
        account_id: account_id.clone(),
        changed: pairs,
    })
}

/// Live JMAP email push monitor.
///
/// Spawn with [`JmapEmailPushMonitor::spawn`]; the returned [`watch::Sender`]
/// cancels the reconnect loop (send `false`). Every email state change is
/// translated to a `NewMail` / `ItemModified` / `ItemDeleted`
/// `NotificationEvent` for the affected folder, matching the existing
/// `Email/changes`-backed sync surface.
pub struct JmapEmailPushMonitor {
    jmap: Arc<JmapClient>,
    username: String,
    password: SecretString,
    subscription_manager: Arc<SubscriptionManager>,
}

impl JmapEmailPushMonitor {
    pub fn new(
        jmap: Arc<JmapClient>,
        username: String,
        password: SecretString,
        subscription_manager: Arc<SubscriptionManager>,
    ) -> Self {
        Self {
            jmap,
            username,
            password,
            subscription_manager,
        }
    }

    /// Spawn the monitor as a background task. The returned sender cancels the
    /// loop when sent `false` (or dropped); dropping it without signalling keeps
    /// the monitor alive indefinitely.
    pub fn spawn(self) -> watch::Sender<bool> {
        let (tx, rx) = watch::channel(true);
        tokio::spawn(async move {
            self.run(rx).await;
        });
        tx
    }

    async fn run(&self, mut cancel: watch::Receiver<bool>) {
        // Resolve the account id once; it is stable for the mailbox.
        let account_id = match self
            .jmap
            .get_account_id(&self.username, &self.password)
            .await
        {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!(target: "jmap_push", error = %e, "cannot resolve JMAP account for push");
                return;
            }
        };

        // Seed the current state so the first change report is a delta, not the
        // whole mailbox.
        let mut current_state = self
            .jmap
            .get_email_state(&account_id, &self.username, &self.password)
            .await
            .unwrap_or_default();

        let mut backoff = Duration::from_secs(1);
        loop {
            if is_cancelled(&cancel) {
                break;
            }
            match self
                .connect_and_stream(&account_id, &mut current_state, &cancel)
                .await
            {
                Ok(()) => {
                    // Server ended the stream cleanly (or shutdown) — reconnect
                    // promptly, then back off if it keeps happening.
                    backoff = Duration::from_secs(1);
                }
                Err(e) => {
                    tracing::debug!(target: "jmap_push", error = %e, "JMAP push stream ended; reconnecting");
                }
            }
            if is_cancelled(&cancel) {
                break;
            }
            tokio::select! {
                _ = cancel.changed() => break,
                _ = tokio::time::sleep(backoff) => {
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
            }
        }
    }

    async fn connect_and_stream(
        &self,
        account_id: &str,
        current_state: &mut String,
        cancel: &watch::Receiver<bool>,
    ) -> Result<(), String> {
        let (url, auth_header) = self
            .jmap
            .push_endpoint(&self.username, &self.password)
            .await
            .map_err(|e| e.to_string())?;

        // Long-lived SSE connection: no overall timeout (a push stream must stay
        // open indefinitely), but keep a connect timeout so a dead endpoint
        // fails fast rather than hanging the reconnect loop.
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;

        let query_url = if url.contains('?') {
            format!("{url}&types=Email")
        } else {
            format!("{url}?types=Email")
        };

        let response = client
            .get(&query_url)
            .header(AUTHORIZATION, &auth_header)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !response.status().is_success() {
            return Err(format!("push endpoint returned HTTP {}", response.status()));
        }

        use futures_util::StreamExt;
        let mut stream = response.bytes_stream();
        let mut carry = String::new();

        while let Some(chunk) = stream.next().await {
            if is_cancelled(cancel) {
                return Ok(());
            }
            let bytes = chunk.map_err(|e| e.to_string())?;
            // Accumulate into a lossy UTF-8 buffer (SSE is UTF-8 text).
            carry.push_str(&String::from_utf8_lossy(&bytes));

            // Extract complete frames, retaining any trailing partial frame.
            let (frames, rest) = parse_sse_frames(&carry);
            carry = rest;

            for frame in frames {
                if is_cancelled(cancel) {
                    return Ok(());
                }
                self.handle_frame(account_id, current_state, &frame).await;
            }
        }

        Ok(())
    }

    async fn handle_frame(
        &self,
        account_id: &str,
        current_state: &mut String,
        frame: &SseEvent,
    ) {
        // A push frame reports which data type changed. We only act on email
        // changes; calendar/contact changes are already served by the existing
        // change-journal poll paths.
        let is_email = match (&frame.event, frame.data.as_deref()) {
            (Some(ev), _) => ev == "Email",
            (_, Some(data)) => parse_state_change(data)
                .is_some_and(|sc| sc.changed.iter().any(|(t, _)| t == "Email")),
            _ => false,
        };
        if !is_email {
            return;
        }

        // Resolve the delta since the last observed state and advance.
        let changes = match self
            .jmap
            .sync_email_changes(account_id, current_state, &self.username, &self.password)
            .await
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(target: "jmap_push", error = %e, "Email/changes failed on push event");
                return;
            }
        };
        *current_state = changes.new_state.clone();

        for email_id in changes.created {
            self.publish_for(&changes.new_state, account_id, &email_id, false)
                .await;
        }
        for email_id in changes.updated {
            self.publish_for(&changes.new_state, account_id, &email_id, true)
                .await;
        }
        // Deleted emails have no folder: publish a Deleted event with an empty
        // folder so store-wide subscriptions still surface the DOM change.
        for email_id in changes.destroyed {
            self.subscription_manager
                .publish(NotificationEvent::ItemDeleted {
                    owner: self.username.clone(),
                    folder_id: String::new(),
                    item_id: email_id,
                });
        }
    }

    async fn publish_for(
        &self,
        state: &str,
        account_id: &str,
        email_id: &str,
        modified: bool,
    ) {
        let Some(email) = self
            .jmap
            .get_email(account_id, email_id, &self.username, &self.password)
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        let folder_id = email
            .mailbox_ids
            .as_ref()
            .and_then(|m| m.keys().next())
            .cloned()
            .unwrap_or_default();
        let change_key = state.to_string();
        let event = if modified {
            NotificationEvent::ItemModified {
                owner: self.username.clone(),
                folder_id,
                item_id: email_id.to_string(),
                change_key,
            }
        } else {
            NotificationEvent::NewMail {
                owner: self.username.clone(),
                folder_id,
                item_id: email_id.to_string(),
                change_key,
            }
        };
        self.subscription_manager.publish(event);
    }
}

/// Read a [`watch::Receiver`] cancellation signal. `false` means cancel.
fn is_cancelled(rx: &watch::Receiver<bool>) -> bool {
    !*rx.borrow()
}

/// Deduplicating registry of live per-mailbox JMAP push monitors.
///
/// The MAPI notification path re-registers frequently (every
/// `RopRegisterNotification` for NewMail), so spawning a fresh push monitor per
/// registration would leak a long-lived SSE connection each time. This registry
/// keeps at most one [`JmapEmailPushMonitor`] per mailbox; `ensure_email_monitor`
/// is a no-op when a live monitor already exists, and lazily re-spawns one when
/// the previous monitor's task ended (detected via the `watch` sender being
/// closed).
pub struct PushMonitorRegistry {
    monitors: dashmap::DashMap<String, watch::Sender<bool>>,
}

impl PushMonitorRegistry {
    pub fn new() -> Self {
        Self {
            monitors: dashmap::DashMap::new(),
        }
    }

    /// Ensure a single live email push monitor exists for `username`, or spawn
    /// one (replaced if the prior monitor died). `password` is the mailbox's
    /// own credential (Basic auth), held in a `SecretString` and only used to
    /// open the JMAP push stream — never persisted or logged.
    pub fn ensure_email_monitor(
        &self,
        jmap: Arc<JmapClient>,
        username: String,
        password: SecretString,
        subscription_manager: Arc<SubscriptionManager>,
    ) {
        if let Some(existing) = self.monitors.get(&username) {
            // A closed sender means every receiver (the monitor task) dropped —
            // the monitor is gone and needs respawning.
            if !existing.is_closed() {
                return;
            }
            drop(existing);
            self.monitors.remove(&username);
        }

        let monitor = JmapEmailPushMonitor::new(jmap, username.clone(), password, subscription_manager);
        let handle = monitor.spawn();
        self.monitors.insert(username, handle);
    }
}

impl Default for PushMonitorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sse_frames_splits_frames_and_joins_data_lines() {
        // A `retry:` frame carries no event/data and is skipped; the trailing
        // `event: Ping` frame is a separate event. Verify multi-line `data:` is
        // joined with `\n` and CRLF-tolerated.
        let stream = "event: Email\ndata: {\"a\":\ndata: 1}\r\n\r\nretry: 3000\r\n\r\nevent: Ping\r\ndata: x\r\n\r\n";
        let (frames, rest) = parse_sse_frames(stream);
        assert!(rest.is_empty(), "no partial frame expected");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].event.as_deref(), Some("Email"));
        assert_eq!(frames[0].data.as_deref(), Some("{\"a\":\n1}"));
        assert_eq!(frames[1].event.as_deref(), Some("Ping"));
        assert_eq!(frames[1].data.as_deref(), Some("x"));
    }

    #[test]
    fn parse_sse_frames_keeps_partial_frame_as_rest() {
        let (frames, rest) = parse_sse_frames("event: Email\ndata: {\"sta");
        assert!(frames.is_empty());
        assert!(rest.contains("event: Email"), "partial frame returned as rest");
    }

    #[test]
    fn parse_state_change_extracts_email_type() {
        let data =
            r#"{"@type":"StateChange","changed":{"acc1":{"Email":"s-42","CalendarEvent":"c-9"}}}"#;
        let sc = parse_state_change(data).expect("valid StateChange");
        assert_eq!(sc.account_id, "acc1");
        assert!(sc.changed.contains(&("Email".to_string(), "s-42".to_string())));
        assert!(sc.changed.contains(&("CalendarEvent".to_string(), "c-9".to_string())));
    }

    #[test]
    fn parse_state_change_rejects_malformed() {
        assert!(parse_state_change("not json").is_none());
        assert!(parse_state_change("{}").is_none());
        assert!(parse_state_change(r#"{"changed":{}}"#).is_none());
    }
}