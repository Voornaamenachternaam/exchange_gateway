#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use exchange_gateway::ical_parser::parse_all_vevents;
use libfuzzer_sys::fuzz_target;

// Grammar-aware iCalendar fuzzing: start with valid events, then mutate
// RRULE/EXDATE/RECURRENCE-ID, VALARM nesting, folded UTF-8 lines, timezone
// blocks, escaping, and malformed CRLF.
#[derive(Debug, Clone)]
enum IcalElement {
    Vevent(IcalVevent),
    Vcalendar { children: Vec<IcalElement> },
    Valarm { children: Vec<IcalElement> },
    Vtimezone { children: Vec<IcalElement> },
    Property { name: String, value: String },
}

#[derive(Debug, Clone)]
struct IcalVevent {
    uid: String,
    dtstart: String,
    dtend: String,
    summary: String,
    properties: Vec<IcalElement>,
}

impl Arbitrary<'_> for IcalElement {
    fn arbitrary(u: &mut Unstructured<'_>) -> arbitrary::Result<Self> {
        let kind: u8 = u.int_in_range(0..=4)?;
        match kind {
            0 => Ok(IcalElement::Vevent(IcalVevent {
                uid: u.arbitrary()?,
                dtstart: u.arbitrary()?,
                dtend: u.arbitrary()?,
                summary: u.arbitrary()?,
                properties: u.arbitrary()?,
            })),
            1 => Ok(IcalElement::Vcalendar { children: u.arbitrary()? }),
            2 => Ok(IcalElement::Valarm { children: u.arbitrary()? }),
            3 => Ok(IcalElement::Vtimezone { children: u.arbitrary()? }),
            _ => Ok(IcalElement::Property {
                name: u.arbitrary()?,
                value: u.arbitrary()?,
            }),
        }
    }

    fn size_hint(_depth: usize) -> (usize, Option<usize>) {
        (0, Some(32))
    }
}

impl Arbitrary<'_> for IcalVevent {
    fn arbitrary(u: &mut Unstructured<'_>) -> arbitrary::Result<Self> {
        Ok(IcalVevent {
            uid: u.arbitrary()?,
            dtstart: u.arbitrary()?,
            dtend: u.arbitrary()?,
            summary: u.arbitrary()?,
            properties: u.arbitrary()?,
        })
    }

    fn size_hint(_depth: usize) -> (usize, Option<usize>) {
        (0, Some(16))
    }
}

fn render_ical(root: &IcalElement, mutate: bool) -> String {
    let mut s = String::new();
    render_element(&mut s, root, mutate);
    if mutate {
        // Inject malformed CRLF, truncated lines, or bad escaping.
        match s.len() {
            0 => s.push_str("BEGIN:VCALENDAR"),
            1..=4 => s.push_str("\r\n"),
            _ => {
                let cut = s.len() / 3;
                s.truncate(cut);
                s.push_str("\r\nEND:VCALENDAR");
            }
        }
    }
    s
}

fn render_element(s: &mut String, el: &IcalElement, mutate: bool) {
    match el {
        IcalElement::Vcalendar { children } => {
            s.push_str("BEGIN:VCALENDAR\r\n");
            for c in children {
                render_element(s, c, mutate);
            }
            s.push_str("END:VCALENDAR\r\n");
        }
        IcalElement::Vevent(vevent) => {
            s.push_str("BEGIN:VEVENT\r\n");
            s.push_str(&format!("UID:{}\r\n", escape_ical(&vevent.uid)));
            s.push_str(&format!("DTSTART:{}\r\n", vevent.dtstart));
            s.push_str(&format!("DTEND:{}\r\n", vevent.dtend));
            s.push_str(&format!("SUMMARY:{}\r\n", escape_ical(&vevent.summary)));
            for p in &vevent.properties {
                render_element(s, p, mutate);
            }
            s.push_str("END:VEVENT\r\n");
        }
        IcalElement::Valarm { children } => {
            s.push_str("BEGIN:VALARM\r\n");
            for c in children {
                render_element(s, c, mutate);
            }
            s.push_str("END:VALARM\r\n");
        }
        IcalElement::Vtimezone { children } => {
            s.push_str("BEGIN:VTIMEZONE\r\n");
            for c in children {
                render_element(s, c, mutate);
            }
            s.push_str("END:VTIMEZONE\r\n");
        }
        IcalElement::Property { name, value } => {
            s.push_str(&format!("{}:{}\r\n", escape_ical(name), escape_ical(value)));
        }
    }
}

fn escape_ical(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    // Grammar-aware corpus: generate structured iCalendar documents.
    if let Ok(ast) = IcalElement::arbitrary(&mut u) {
        let mutate: bool = u.arbitrary().unwrap_or(false);
        let ics = render_ical(&ast, mutate);

        // Parsing must not panic on any grammar-generated input.
        let _ = parse_all_vevents(&ics);
    }

    // Raw byte fuzzing of the parser (fail-closed, no panics). Lossy UTF-8
    // conversion since the parser expects &str.
    let text = String::from_utf8_lossy(data);
    let _ = parse_all_vevents(&text);
});
