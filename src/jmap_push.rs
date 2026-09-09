// src/jmap_push.rs
//
// Live JMAP push subscription (`urn:ietf:params:jmap:push`, RFC 8620 §7).
//
// The gateway's email-change detection historically relied on *client-driven*
// polling: a mailbox only learned about new mail when a client issued an
// `Email/changes`-backed Sync / Ping. That leaves the MAPI `NotificationWait`
// / `RopNotify` path (New Outlook) and EAS `Ping` dependent on the client's
// poll cadence, so "new mail" can lag by the poll interval (audit gap #11).
//
// This module replaces that reliance with a live `text/event-stream` (SSE)
// subscription to Stalwart's JMAP push endpoint. Per RFC 8620 §7.3 the server
// emits an event *named* `state` whose data is a `StateChange` object
// (§7.1) whenever the account's `Email` state advances. On each event the
// monitor resolves the delta through the existing `Email/changes` + `Email/get`
// path (already used by EWS/EAS sync) and publishes `NotificationEvent::NewMail`
// / `ItemModified` / `ItemDeleted` into the shared subscription manager.
// The credentials are the user's own (Basic auth), obtained in-band at the
// point the client registers for notifications — never persisted.
//
// SSE wire format (WHATWG HTML "server-sent events"):
//   event: state
//   data: {"@type":"StateChange","changed":{"<accountId>":{"Email":"<state>"}}}
//
// A frame is terminated by a blank line; `data:` lines within a frame are
// joined with a single `\n`. Reconnects use bounded exponential backoff, an
// idle timeout guards half-open connections, and the monitor self-terminates
// via its [`CancellationToken`] when the last notification sink for the
// mailbox is released (see [`PushMonitorRegistry::release_email_monitor`]).

use crate::jmap::JmapClient;
use crate::notifications::{NotificationEvent, SubscriptionManager};
use reqwest::header::AUTHORIZATION;
use secrecy::SecretString;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Upper bound on the accumulated, unterminated SSE frame text. A single frame
/// without a trailing blank line must never grow the buffer without limit
/// (denial-of-service guard); exceeding this reconnects the stream.
const MAX_SSE_FRAME_BYTES: usize = 64 * 1024;

/// Reconnect idle guard: if the EventSource produces no bytes (and no
/// heartbeat comment) for this long, the connection is treated as half-open and
/// re-established. Deliberately generous — a genuinely quiet mailbox must not
/// be torn down.
const SSE_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// One parsed server-sent event frame (event type and its `data:` payload).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: Option<String>,
}

/// Parse a raw `text/event-stream` chunk into zero or more complete frames.
///
/// Frames are separated by a blank line. Within a frame, `event:` and `data:`
/// fields are captured; multiple `data:` lines are joined with `\n` (per the
/// WHATWG incrementing rules). Comment lines (`:` prefix) and `retry:`/`id:`
/// fields are ignored. Partial frames (no trailing blank line) are returned in
/// `rest` so the caller can prepend it to the next chunk.
pub fn parse_sse_frames(input: &str) -> (Vec<SseEvent>, String) {
    let mut frames = Vec::new();

    // Normalise CRLF / lone-CR line endings to `\n` so the blank-line frame
    // separator can be located uniformly (SSE allows any of `\r\n`, `\n`, `\r`).
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");

    // Locate every complete frame up to the last blank-line separator; the
    // trailing segment after it is a partial frame returned as `rest`.
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
            if line.starts_with(':') {
                // Comment / keep-alive heartbeat — ignored.
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
        if !data_lines.is_empty() {
            frames.push(SseEvent {
                event,
                data: Some(data_lines.join("\n")),
            });
        }
    }

    (frames, rest)
}

/// A parsed JMAP `StateChange` push payload (RFC 8620 §7.1): the account id and
/// the `(data_type, new_state)` pairs whose state advanced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateChange {
    pub account_id: String,
    /// `(data_type, new_state)` pairs — e.g. `("Email", "s-123")`.
    pub changed: Vec<(String, String)>,
}

/// Parse a JMAP `StateChange` JSON `data` payload into [`StateChange`].
///
/// The payload is `{ "@type":"StateChange", "changed":
/// { "<accountId>": { "<type>": "<state>" } } }`. The `@type` member MUST be
/// `StateChange` (RFC 8620 §7.1); other-shaped objects are rejected so an
/// unrelated EventSource payload can never be mistaken for a state change.
/// Entries whose value is not a state string are skipped rather than discarding
/// the whole payload. Returns `None` for malformed / non-`StateChange` data or
/// when no valid pair survives.
pub fn parse_state_change(data: &str) -> Option<StateChange> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    if value.get("@type").and_then(|t| t.as_str()) != Some("StateChange") {
        return None;
    }
    let changed = value.get("changed")?.as_object()?;
    let (account_id, types) = changed.iter().next()?;
    let mut pairs = Vec::new();
    for (data_type, state) in types.as_object()? {
        if let Some(s) = state.as_str() {
            pairs.push((data_type.clone(), s.to_string()));
        }
    }
    if pairs.is_empty() {
        return None;
    }
    Some(StateChange {
        account_id: account_id.clone(),
        changed: pairs,
    })
}

/// Decide whether an SSE frame represents an email state change.
///
/// RFC 8620 §7.3 pushes an event *named* `state` (not `Email`); the actual
/// signal is the parsed `StateChange` data carrying an `Email` entry. A frame
/// whose SSE event name is already `Email` (older/lenient servers) is accepted
/// for tolerance, but the authoritative check is the data payload. Extracted as
/// a free function so it is unit-testable without a live `JmapClient`.
fn is_email_frame(frame: &SseEvent) -> bool {
    if let Some(data) = frame.data.as_deref()
        && let Some(sc) = parse_state_change(data)
    {
        return sc.changed.iter().any(|(t, _)| t == "Email");
    }
    frame.event.as_deref() == Some("Email")
}

/// Outcome of one EventSource connection attempt.
enum StreamEnd {
    /// Server closed the stream cleanly (EOF) or shutdown was requested.
    Clean,
    /// A forced reconnect guard fired (idle/half-open or oversized frame).
    Reconnect,
    /// The connection failed; the enclosed message is the cause.
    Error(String),
}

/// Live JMAP email push monitor.
///
/// Spawn with [`JmapEmailPushMonitor::spawn`]; the returned [`CancellationToken`]
/// cancels the reconnect loop when `.cancel()` is invoked. Every email state
/// change is translated to a `NewMail` / `ItemModified` / `ItemDeleted`
/// `NotificationEvent` for the affected folder.
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

    /// Spawn the monitor as a background task. The returned [`CancellationToken`]
    /// tears the reconnect loop down when `.cancel()` is called (e.g. by the
    /// registry when the last notification sink for the mailbox is released).
    pub fn spawn(self) -> CancellationToken {
        let token = CancellationToken::new();
        let task_token = token.clone();
        tokio::spawn(async move {
            self.run(task_token).await;
        });
        token
    }

    async fn run(&self, cancel: CancellationToken) {
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

        let mut current_state = self
            .jmap
            .get_email_state(&account_id, &self.username, &self.password)
            .await
            .unwrap_or_default();

        let mut backoff = Duration::from_secs(1);
        loop {
            if cancel.is_cancelled() {
                break;
            }
            match self
                .connect_and_stream(&account_id, &mut current_state, &cancel)
                .await
            {
                StreamEnd::Clean | StreamEnd::Reconnect => {}
                StreamEnd::Error(e) => {
                    tracing::debug!(target: "jmap_push", error = %e, "JMAP push stream ended; reconnecting");
                }
            }
            if cancel.is_cancelled() {
                break;
            }
            tokio::select! {
                _ = cancel.cancelled() => break,
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
        cancel: &CancellationToken,
    ) -> StreamEnd {
        let (url, auth_header) = match self
            .jmap
            .push_endpoint(&self.username, &self.password)
            .await
        {
            Ok(v) => v,
            Err(e) => return StreamEnd::Error(e.to_string()),
        };

        let client = match reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
        {
            Ok(c) => c,
            Err(e) => return StreamEnd::Error(e.to_string()),
        };

        let query_url = if url.contains('?') {
            format!("{url}&types=Email")
        } else {
            format!("{url}?types=Email")
        };

        let response = match client
            .get(&query_url)
            .header(AUTHORIZATION, &auth_header)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return StreamEnd::Error(e.to_string()),
        };

        if !response.status().is_success() {
            return StreamEnd::Error(format!("push endpoint returned HTTP {}", response.status()));
        }

        use futures_util::StreamExt;
        let mut stream = response.bytes_stream();
        let mut carry = String::new();

        loop {
            tokio::select! {
                _ = cancel.cancelled() => return StreamEnd::Clean,
                next = stream.next() => {
                    match next {
                        None => return StreamEnd::Clean,
                        Some(Err(e)) => return StreamEnd::Error(e.to_string()),
                        Some(Ok(bytes)) => {
                            carry.push_str(&String::from_utf8_lossy(&bytes));
                            if carry.len() > MAX_SSE_FRAME_BYTES {
                                return StreamEnd::Reconnect;
                            }
                            let (frames, rest) = parse_sse_frames(&carry);
                            carry = rest;
                            for frame in frames {
                                if cancel.is_cancelled() {
                                    return StreamEnd::Clean;
                                }
                                self.handle_frame(account_id, current_state, &frame).await;
                            }
                        }
                    }
                }
                _ = tokio::time::sleep(SSE_IDLE_TIMEOUT) => {
                    return StreamEnd::Reconnect;
                }
            }
        }
    }

    async fn handle_frame(&self, account_id: &str, current_state: &mut String, frame: &SseEvent) {
        if !is_email_frame(frame) {
            return;
        }

        if current_state.is_empty()
            && let Ok(seeded) = self
                .jmap
                .get_email_state(account_id, &self.username, &self.password)
                .await
        {
            *current_state = seeded;
        }

        let mut created = Vec::new();
        let mut updated = Vec::new();
        let mut destroyed = Vec::new();
        let mut since = current_state.clone();
        loop {
            let changes = match self
                .jmap
                .sync_email_changes(account_id, &since, &self.username, &self.password)
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(target: "jmap_push", error = %e, "Email/changes failed on push event");
                    return;
                }
            };
            created.extend(changes.created);
            updated.extend(changes.updated);
            destroyed.extend(changes.destroyed);
            since = changes.new_state.clone();
            if !changes.has_more_changes {
                break;
            }
        }
        *current_state = since;

        let mut lookup = Vec::with_capacity(created.len() + updated.len());
        lookup.extend(created.iter().cloned());
        lookup.extend(updated.iter().cloned());
        let emails = self
            .jmap
            .get_emails(account_id, &lookup, None, &self.username, &self.password)
            .await
            .unwrap_or_default();
        let mut emails = emails.into_iter();
        for email_id in created {
            let email = emails.next();
            self.publish_for(&self.username, email_id, email, current_state, false);
        }
        for email_id in updated {
            let email = emails.next();
            self.publish_for(&self.username, email_id, email, current_state, true);
        }
        for email_id in destroyed {
            self.subscription_manager
                .publish(NotificationEvent::ItemDeleted {
                    owner: self.username.clone(),
                    folder_id: String::new(),
                    item_id: email_id,
                });
        }
    }

    fn publish_for(
        &self,
        owner: &str,
        email_id: String,
        email: Option<crate::jmap::JmapEmail>,
        change_key: &str,
        modified: bool,
    ) {
        let Some(email) = email else {
            return;
        };
        let folder_id = email
            .mailbox_ids
            .as_ref()
            .and_then(|m| m.keys().min())
            .cloned()
            .unwrap_or_default();
        let change_key = change_key.to_string();
        let event = if modified {
            NotificationEvent::ItemModified {
                owner: owner.to_string(),
                folder_id,
                item_id: email_id,
                change_key,
            }
        } else {
            NotificationEvent::NewMail {
                owner: owner.to_string(),
                folder_id,
                item_id: email_id,
                change_key,
            }
        };
        self.subscription_manager.publish(event);
    }
}

struct MonitorHandle {
    cancel: CancellationToken,
    sinks: AtomicUsize,
}

/// Deduplicating registry of live per-mailbox JMAP push monitors.
///
/// Keeps at most one [`JmapEmailPushMonitor`] per mailbox: `ensure_email_monitor`
/// atomically increments a refcount (spawning only when no live monitor exists),
/// and `release_email_monitor` decrements it, cancelling the monitor — releasing
/// its SSE connection and in-memory credential — when the last sink is gone.
pub struct PushMonitorRegistry {
    monitors: dashmap::DashMap<String, MonitorHandle>,
}

impl PushMonitorRegistry {
    pub fn new() -> Self {
        Self {
            monitors: dashmap::DashMap::new(),
        }
    }

    pub fn ensure_email_monitor(
        &self,
        jmap: Arc<JmapClient>,
        username: String,
        password: SecretString,
        subscription_manager: Arc<SubscriptionManager>,
    ) {
        use dashmap::mapref::entry::Entry;
        match self.monitors.entry(username.clone()) {
            Entry::Vacant(v) => {
                let monitor =
                    JmapEmailPushMonitor::new(jmap, username, password, subscription_manager);
                let cancel = monitor.spawn();
                v.insert(MonitorHandle {
                    cancel,
                    sinks: AtomicUsize::new(1),
                });
            }
            Entry::Occupied(o) => {
                o.get().sinks.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn release_email_monitor(&self, username: &str) {
        let should_cancel = match self.monitors.get(username) {
            Some(handle) => handle.sinks.fetch_sub(1, Ordering::AcqRel) == 1,
            None => return,
        };
        if should_cancel && let Some((_, handle)) = self.monitors.remove(username) {
            handle.cancel.cancel();
        }
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
        assert!(
            rest.contains("event: Email"),
            "partial frame returned as rest"
        );
    }

    #[test]
    fn parse_state_change_extracts_email_type() {
        let data =
            r#"{"@type":"StateChange","changed":{"acc1":{"Email":"s-42","CalendarEvent":"c-9"}}}"#;
        let sc = parse_state_change(data).expect("valid StateChange");
        assert_eq!(sc.account_id, "acc1");
        assert!(
            sc.changed
                .contains(&("Email".to_string(), "s-42".to_string()))
        );
        assert!(
            sc.changed
                .contains(&("CalendarEvent".to_string(), "c-9".to_string()))
        );
    }

    #[test]
    fn parse_state_change_requires_type_marker() {
        assert!(parse_state_change(r#"{"changed":{"acc1":{"Email":"s-1"}}}"#).is_none());
        assert!(
            parse_state_change(r#"{"@type":"Foo","changed":{"acc1":{"Email":"s-1"}}}"#).is_none()
        );
    }

    #[test]
    fn parse_state_change_skips_non_string_values() {
        let data = r#"{"@type":"StateChange","changed":{"acc1":{"Email":"s-1","Other":3}}}"#;
        let sc = parse_state_change(data).expect("valid StateChange");
        assert_eq!(sc.changed, vec![("Email".to_string(), "s-1".to_string())]);
    }

    #[test]
    fn parse_state_change_rejects_malformed() {
        assert!(parse_state_change("not json").is_none());
        assert!(parse_state_change("{}").is_none());
        assert!(parse_state_change(r#"{"changed":{}}"#).is_none());
    }

    #[test]
    fn is_email_frame_uses_state_event_data() {
        let frame = SseEvent {
            event: Some("state".to_string()),
            data: Some(
                r#"{"@type":"StateChange","changed":{"acc1":{"Email":"s-99"}}}"#.to_string(),
            ),
        };
        assert!(is_email_frame(&frame));

        let non_email = SseEvent {
            event: Some("state".to_string()),
            data: Some(
                r#"{"@type":"StateChange","changed":{"acc1":{"CalendarEvent":"c-1"}}}"#.to_string(),
            ),
        };
        assert!(!is_email_frame(&non_email));
    }

    #[test]
    fn is_email_frame_tolerates_legacy_email_name() {
        let frame = SseEvent {
            event: Some("Email".to_string()),
            data: None,
        };
        assert!(is_email_frame(&frame));
    }
}
