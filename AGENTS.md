# exchange_gateway - agent notes

## GitHub push auth (IMPORTANT - ghu_ app-installation tokens)
The `GITHUB_TOKEN` env var in this workspace is a GitHub App installation token
(prefix `ghu_`, 40 chars). It does NOT authenticate the usual ways:

- `curl -H "Authorization: token $GITHUB_TOKEN"` -> 401 (wrong scheme).
- `git push https://$GITHUB_TOKEN@github.com/owner/repo.git` -> 401
  "Invalid username or token. Password authentication is not supported."
  (git sends the token as a basic-auth username, which GitHub rejects for app tokens.)
- `git -c http.extraHeader="Authorization: Bearer $GITHUB_TOKEN" push` -> 401
  "invalid credentials" via the default helper path.

What DOES work (use this exact method - it's how `gh` pushes app tokens):

```sh
# (A) API calls / gh-style: use Bearer (not "token")
curl -H "Authorization: Bearer $GITHUB_TOKEN"   https://api.github.com/repos/Voornaamenachternaam/exchange_gateway

# (B) git push: pass the token as the password with username x-access-token
GIT_TERMINAL_PROMPT=0 GIT_ASKPASS=/bin/true git   -c credential.helper='!f() { echo "username=x-access-token"; echo "password=$GITHUB_TOKEN"; }; f'   push https://github.com/Voornaamenachternaam/exchange_gateway.git <branch>
```

Equivalent one-liner using `gh` as the credential helper:
`printf 'protocol=https\nhost=github.com\n\n' | gh auth git-credential get`
returns `username=x-access-token` + `password=<token>`.

So: if a `ghu_` token 401s with `Authorization: token ...` or via the
`https://<token>@host` URL, switch to `Authorization: Bearer` for REST and
`username=x-access-token / password=<token>` for git. Don't conclude the
token is invalid until you've tried the Bearer scheme.

## Build toolchain (IMPORTANT - persistent location)
The container's `$HOME` (and therefore `~/.cargo`/`~/.rustup`) is volatile and
gets wiped between shell sessions. To avoid re-installing Rust every command:

- Rust is installed persistently under `/workspace`:
  - `CARGO_HOME=/workspace/.cargo`
  - `RUSTUP_HOME=/workspace/.rustup`
  - Toolchain: Rust 1.96.1 (matches the project requirement).
- ALWAYS prefix build/test commands with:
  `export CARGO_HOME=/workspace/.cargo RUSTUP_HOME=/workspace/.rustup && . /workspace/.cargo/env`
- If `cargo` is reported missing, reinstall once with:
  `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.96.1 --profile minimal`
  (deploys into `/workspace/.cargo` so it survives future sessions).

## Build / test
- Clean build (no warnings, no suppression): `RUSTFLAGS="-D warnings" cargo build --bin exchange_gateway`
- Tests: `RUSTFLAGS="-D warnings" cargo test --lib` (lib unit tests) + `cargo test --test snapshot_tests` (insta snapshots).
- Long compiles: run cargo in the background and poll; redirect logs to a file
  under `/workspace/` (NOT `/tmp` - `/tmp` is also volatile across sessions).

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
