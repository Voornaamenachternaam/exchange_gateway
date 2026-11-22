#![no_main]

use exchange_gateway::mapi::rops::Buf;
use libfuzzer_sys::fuzz_target;

// Fuzz ROP (Remote Operation) decoding — a custom, untrusted binary-input
// path with no prior fuzz coverage. Fail-closed: decoding must never panic
// on attacker-controlled bytes; every length is bounds-checked.
fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }

    let mut buf = Buf::new(data);

    // Try decoding common scalar types from arbitrary positions.
    let _ = buf.take_u8();
    let _ = buf.take_u16_le();
    let _ = buf.take_u32_le();
    let _ = buf.take_u64_le();

    // Attempt to decode a full ROP logon request; failures must be clean
    // errors, never panics.
    let _ = exchange_gateway::mapi::rops::RopLogonRequest::decode(&mut buf);

    // Consume remaining bytes as raw — must not panic.
    let _ = buf.remaining();
});
