// src/mapi/mod.rs
//
// MAPI over HTTP (MS-OXCMAPIHTTP) server-side implementation.
//
// This module implements the MS-OXCMAPIHTTP transport: the RPC channel that
// New Outlook for Windows (and classic Outlook for Windows) uses to read and
// synchronise mailbox data. The MAPI/HTTP surface is layered on top of:
//   * MS-OXCMAPIHTTP — the HTTP transport (Connect, Disconnect, Execute RPCs,
//     X-RequestType / X-ClientInfo headers, application/mapi-http framing).
//   * MS-OXCROPS     — the Remote Operation (ROP) set layered on the buffer.
//   * MS-OXCDATA     — the MAPI property/type universe.
//   * MS-OXCFXICS    — the Fast Transfer Stream codec for bulk message/folder
//     synchronisation (FXICOP incremental-change streams).
//
// Phase 0 scope (this file tree):
//   * Typed, parse-fails-closed codecs for the MAPI/HTTP transport, the ROP
//     buffer framing, the property universe, and the FXICS transfer stream.
//   * Per-codec unit + `proptest` round-trip coverage.
//   * The `/mapi/{mailboxGuid}/{...}` HTTP route wired into the axum router,
//     guarded by `GATEWAY_MAPI_ENABLED` (default off).
//   * Basic-auth-backed `RopLogon` over the existing `AuthVerifier`, plus a
//     pluggable bearer/HMA token validator (`oidc::TokenVerifier`) so the
//     HMA path can be enabled without touching the transport layer.
//   * `RopLogon`, `RopGetContentsTable`, `RopGetHierarchyTable`, `RopQueryRows`,
//     `RopOpenMessage`, `RopGetProps`, and `RopSetReadFlags` backed by the
//     existing JMAP/CalDAV/CardDAV backends — the mail read + calendar/
//     contacts enumeration surface classic Outlook uses over MAPI/HTTP.
//
// All untrusted input is length-checked and parsed with fail-closed bounds;
// integer conversions on attacker-controlled lengths use `u32::try_from` and
// `usize::try_from` (no `as` casts on untrusted data). Sessions are held in a
// `parking_lot`-guarded map with an idle TTL and are zeroized on drop.

pub mod converters;
pub mod data;
pub mod fxics;
pub mod handler;
pub mod logon;
pub mod restrict;
pub mod rops;
pub mod session;
pub mod store;
pub mod transport;

pub use handler::handle as handle_request;
pub use transport::{MapiRequest, MapiRequestType, MapiResponse, RpcKind};
