#![no_main]

use exchange_gateway::mapi::tnef::parse;
use libfuzzer_sys::fuzz_target;

// Fuzz TNEF (Transport Neutral Encapsulation Format) parsing — a custom,
// untrusted binary-input path with no prior fuzz coverage. Fail-closed:
// parsing must never panic on attacker-controlled winmail.dat blobs.
fuzz_target!(|data: &[u8]| {
    // TNEF parsing must not panic on any input.
    let _ = parse(data);
});
