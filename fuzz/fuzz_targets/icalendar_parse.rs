#![no_main]

use exchange_gateway::calendar::parse_ics_event;
use exchange_gateway::ical_parser::{parse_all_vevents, parse_property_lines, unfold_ical_content};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    let Ok(input) = std::str::from_utf8(bytes) else {
        return;
    };
    let unfolded = unfold_ical_content(input);
    let _ = parse_property_lines(&unfolded);
    let _ = parse_all_vevents(input);
    let _ = parse_ics_event(input);
});
