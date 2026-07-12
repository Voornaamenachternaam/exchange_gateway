# exchange_gateway — agent notes

## Build toolchain (IMPORTANT — persistent location)
The container's `$HOME` (and therefore `~/.cargo`/`~/.rustup`) is **volatile** and
gets wiped between shell sessions. To avoid re-installing Rust every command:

- Rust is installed persistently under `/workspace`:
  - `CARGO_HOME=/workspace/.cargo`
  - `RUSTUP_HOME=/workspace/.rustup`
  - Toolchain: Rust 1.96.1 (matches the project requirement).
- **Always** prefix build/test commands with:
  ```sh
  export CARGO_HOME=/workspace/.cargo RUSTUP_HOME=/workspace/.rustup && . /workspace/.cargo/env
  ```
- If `cargo` is reported missing, reinstall once with:
  ```sh
  export CARGO_HOME=/workspace/.cargo RUSTUP_HOME=/workspace/.rustup && \
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- -y --default-toolchain 1.96.1 --profile minimal
  ```
  (deploys into `/workspace/.cargo` so it survives future sessions).

## Build / test
- Clean build (no warnings allowed, no suppression): `RUSTFLAGS="-D warnings" cargo build --bin exchange_gateway`
- Tests: `RUSTFLAGS="-D warnings" cargo test --lib` (lib unit tests) and `cargo test --test snapshot_tests` (insta snapshots).
- Long compile times: run cargo in the background and poll; redirect logs to a
  file under `/workspace/` (NOT `/tmp` — `/tmp` is also volatile across sessions).

## Architecture notes
- EWS notifications (MS-OXWSNTIF) live in `src/notifications.rs`
  (`SubscriptionManager`, `NotificationEvent`) and are rendered/wired in
  `src/ews.rs` (`handle_subscribe`, `handle_unsubscribe`, `handle_get_events`,
  `handle_get_streaming_events`, `publish_event`, `render_notification`,
  `render_notification_event`, `encode_watermark`/`decode_watermark`,
  `parse_subscription_request`). Item CRUD handlers (Create/Update/Delete/Move/Copy
  for email, calendar, contact) publish `NotificationEvent`s via `publish_event`.
- Backend: JMAP for email read/sync/send, SMTP for send + iMIP meeting transport,
  CalDAV authoritative for calendar/free-busy, CardDAV for contacts. Do NOT use
  IMAP (vestigial/unused).
