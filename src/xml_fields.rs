//! Single-pass extraction of element text from a request document.
//!
//! EAS/EWS handlers read several fields out of the same request body, and the
//! per-tag helpers restart a quick-xml reader at the top of the document for
//! every field, so a handler that reads `k` fields pays `k` prefix scans — and a
//! field that is ABSENT costs a full scan, because the helper only stops at
//! Eof. A `SendMail` carrying 40 MiB of `MIMEData` is the worst case: the
//! fields a SendMail body does not have (CollectionId, ItemId) each tokenise the
//! whole MIME blob.
//!
//! [`RequestFields`] parses the document ONCE and records, per element name,
//! the byte span of the value. Text is materialised only when it is actually
//! asked for, so a 40 MiB element costs nothing unless the caller reads it, and
//! an element that is pure text (no child element, CDATA, reference, comment,
//! PI or doctype) is returned as a slice of the body with no intermediate
//! string at all.
//!
//! Two value rules are reproduced exactly, because callers depend on them:
//!
//! * [`RequestFields::first`] — the [`first_tag_text`] rule: a nested Start of
//!   the SAME local name clears the accumulator, the FIRST End of that name
//!   returns the value, and only the finished value is trimmed (so
//!   `R &amp; D` keeps the spaces around the reference).
//! * [`RequestFields::first_outer`] — the [`first_outer_text`] rule: the
//!   OUTERMOST first element of that name, untrimmed.
//!
//! Both fall back to the original per-tag implementation whenever the index
//! cannot answer — a document that fails to parse, or a name the index did not
//! record — so a `RequestFields` lookup is never less correct than the helper
//! it replaces. The index is an accelerator, never a correctness dependency.

use quick_xml::Reader;
use quick_xml::events::Event;
use std::collections::HashMap;

use crate::util::resolve_xml_reference;

/// Trim a `String` in place, without the extra allocation `trim().to_string()`
/// makes on every extracted field.
pub(crate) fn trim_owned(mut value: String) -> String {
    let end = value.trim_end().len();
    value.truncate(end);
    let start = value.len() - value.trim_start().len();
    if start > 0 {
        value.drain(..start);
    }
    value
}

/// First value of `tag` by the EAS rule (see [`RequestFields::first`]).
///
/// This is the reference implementation: [`RequestFields`] reproduces it and
/// falls back to it whenever its index cannot answer.
pub(crate) fn first_tag_text(xml: &str, tag: &[u8]) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    // Text is NOT trimmed per event: quick-xml emits a separate `Text` event
    // around every character reference, so per-event trimming would delete the
    // spaces that surround an entity (`R &amp; D` -> `R&D`). Only the finished
    // value's boundaries are trimmed.
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut inside = false;
    let mut value = String::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().local_name().as_ref().as_bytes() == tag => {
                inside = true;
                value.clear();
            }
            Ok(Event::Text(t)) if inside => value.push_str(t.as_ref()),
            Ok(Event::CData(t)) if inside => value.push_str(t.as_ref()),
            Ok(Event::GeneralRef(r)) if inside => {
                value.push_str(&resolve_xml_reference(r.as_ref()))
            }
            Ok(Event::End(e)) if e.name().local_name().as_ref().as_bytes() == tag => {
                return Some(trim_owned(value));
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

/// First value of the element whose local name is `local`, taking the
/// OUTERMOST occurrence and NOT trimming (the `extract_first_tag_text_local`
/// rule used by the raw-XML SendMail path).
///
/// `first_tag_text` cannot serve this: it returns the INNERMOST value of a
/// nested same-name element and trims the result.
pub(crate) fn first_outer_text(xml: &str, local: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut depth = 0usize;
    let mut capturing = false;
    let mut out = String::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if capturing {
                    depth += 1;
                } else if e.name().local_name().as_ref() == local {
                    capturing = true;
                }
            }
            Ok(Event::End(_)) => {
                if capturing {
                    if depth == 0 {
                        return Some(out);
                    }
                    depth -= 1;
                }
            }
            Ok(Event::Text(e)) => {
                if capturing {
                    out.push_str(e.as_ref());
                }
            }
            Ok(Event::CData(e)) => {
                if capturing {
                    // CDATA is literal by definition — no reference decoding.
                    out.push_str(e.as_ref());
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if capturing {
                    out.push_str(&resolve_xml_reference(r.as_ref()));
                }
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

/// How the materialised text is trimmed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Trim {
    /// Trim the finished value only (`first_tag_text`).
    Value,
    /// No trimming at all (`first_outer_text`).
    None,
}

/// Byte span of one element's content.
#[derive(Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
    /// The content is pure text, so `body[start..end]` IS the element's value:
    /// no child element, CDATA, reference, comment, PI or doctype.
    plain: bool,
}

impl Span {
    fn text(&self, body: &str, trim: Trim) -> String {
        let raw = body.get(self.start..self.end).unwrap_or_default();
        let text = if self.plain {
            raw.to_string()
        } else {
            decode_fragment(raw)
        };
        match trim {
            Trim::Value => trim_owned(text),
            Trim::None => text,
        }
    }
}

/// Text of a content fragment that is not pure text: every `Text`, `CData` and
/// reference event at ANY depth, verbatim and without per-event trimming.
///
/// A fragment is exactly one element's content, so collecting at any depth is
/// what the per-tag helpers do for the same element (they append descendant
/// text to the accumulator and only react to the target element's own tags).
fn decode_fragment(raw: &str) -> String {
    let mut reader = Reader::from_str(raw);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut out = String::with_capacity(raw.len());
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Text(t)) => out.push_str(t.as_ref()),
            Ok(Event::CData(t)) => out.push_str(t.as_ref()),
            Ok(Event::GeneralRef(r)) => out.push_str(&resolve_xml_reference(r.as_ref())),
            Ok(Event::Eof) | Err(_) => return out,
            _ => {}
        }
        buf.clear();
    }
}

/// One element open at the current point of the pass.
struct Open<'a> {
    /// LOCAL name, borrowed from the body: both value rules match on the local
    /// name, so `AirSyncBase:Body` and `Body` are the same field.
    name: &'a str,
    content_start: usize,
    /// No child element / CDATA / reference / comment / PI / doctype seen yet.
    plain: bool,
}

/// A request document indexed in ONE pass.
pub(crate) struct RequestFields<'a> {
    body: &'a str,
    /// First-closing occurrence of each local name (`first_tag_text` rule).
    inner: HashMap<&'a str, Span>,
    /// Outermost occurrence of each local name (`first_outer_text` rule).
    outer: HashMap<&'a str, Span>,
    /// The pass reached Eof with every element closed. Only then does a miss
    /// mean "this document has no such element"; otherwise the index is
    /// partial and every lookup falls back to the per-tag helper.
    complete: bool,
}

impl<'a> RequestFields<'a> {
    /// Index `body` in one pass.
    pub(crate) fn parse(body: &'a str) -> Self {
        let mut fields = Self {
            body,
            inner: HashMap::new(),
            outer: HashMap::new(),
            complete: false,
        };
        fields.index();
        fields
    }

    fn index(&mut self) {
        let mut reader = Reader::from_str(self.body);
        reader.config_mut().trim_text(false);
        let mut stack: Vec<Open<'a>> = Vec::new();
        // Stack index of the OUTERMOST open occurrence of each name, so the
        // outer span can be recorded when that exact element closes.
        let mut outer_at: HashMap<&'a str, usize> = HashMap::new();
        let mut inner: HashMap<&'a str, Span> = HashMap::new();
        let mut outer: HashMap<&'a str, Span> = HashMap::new();
        loop {
            // `buffer_position` reports the end of the last read event, so the
            // position captured before a read is that event's start offset.
            let before = reader.buffer_position() as usize;
            let event = reader.read_event();
            let after = reader.buffer_position() as usize;
            match event {
                Ok(Event::Start(_)) => {
                    let Some(name) = self.tag_local_name(before, after) else {
                        return;
                    };
                    if let Some(parent) = stack.last_mut() {
                        parent.plain = false;
                    }
                    outer_at.entry(name).or_insert(stack.len());
                    stack.push(Open {
                        name,
                        content_start: after,
                        plain: true,
                    });
                }
                Ok(Event::End(_)) => {
                    let Some(name) = self.tag_local_name(before, after) else {
                        return;
                    };
                    let Some(top) = stack.pop() else { return };
                    // Mismatched end tags are rejected by quick-xml before they
                    // reach this arm; treating one as "no index" keeps the
                    // fallback path authoritative if that ever changes.
                    if top.name != name {
                        return;
                    }
                    let span = Span {
                        start: top.content_start,
                        end: before,
                        plain: top.plain,
                    };
                    inner.entry(top.name).or_insert(span);
                    if outer_at.get(top.name) == Some(&stack.len()) {
                        outer_at.remove(top.name);
                        outer.entry(top.name).or_insert(span);
                    }
                }
                // A child element (even a self-closing one) makes the parent's
                // content markup, not text.
                Ok(Event::Empty(_)) => {
                    if let Some(parent) = stack.last_mut() {
                        parent.plain = false;
                    }
                }
                Ok(Event::Text(_)) => {}
                Ok(Event::CData(_))
                | Ok(Event::GeneralRef(_))
                | Ok(Event::Comment(_))
                | Ok(Event::PI(_))
                | Ok(Event::DocType(_))
                | Ok(Event::Decl(_)) => {
                    if let Some(parent) = stack.last_mut() {
                        parent.plain = false;
                    }
                }
                Ok(Event::Eof) => {
                    if stack.is_empty() {
                        self.inner = inner;
                        self.outer = outer;
                        self.complete = true;
                    }
                    return;
                }
                Err(_) => return,
            }
        }
    }

    /// LOCAL name of the tag whose raw source is `body[start..end]`, borrowed
    /// from the body.
    ///
    /// quick-xml ties `local_name()` to the event value, not to the input, so
    /// the name is sliced straight out of the document instead: it starts right
    /// after `<` (and `/` for an end tag) and runs to the first whitespace,
    /// `>` or `/`. `None` when that slice is not a char boundary, which only a
    /// malformed tag can produce.
    fn tag_local_name(&self, start: usize, end: usize) -> Option<&'a str> {
        let bytes = self.body.as_bytes();
        let mut i = start.checked_add(1)?;
        if bytes.get(i) == Some(&b'/') {
            i += 1;
        }
        let name_start = i;
        while i < end && !matches!(bytes.get(i)?, b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/') {
            i += 1;
        }
        let full = std::str::from_utf8(bytes.get(name_start..i)?).ok()?;
        Some(full.rsplit(':').next().unwrap_or(full))
    }

    /// First value of `tag`, by the [`first_tag_text`] rule.
    pub(crate) fn first(&self, tag: &[u8]) -> Option<String> {
        let key = std::str::from_utf8(tag).ok();
        match key.and_then(|k| self.inner.get(k)) {
            Some(span) => Some(span.text(self.body, Trim::Value)),
            None if self.complete => None,
            None => first_tag_text(self.body, tag),
        }
    }

    /// First value of the element whose local name is `local`, by the
    /// [`first_outer_text`] rule (outermost occurrence, untrimmed).
    pub(crate) fn first_outer(&self, local: &str) -> Option<String> {
        match self.outer.get(local) {
            Some(span) => Some(span.text(self.body, Trim::None)),
            None if self.complete => None,
            None => first_outer_text(self.body, local),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Documents that exercise every shape the per-tag helper distinguishes:
    /// plain text, nested same-name elements, child elements, references,
    /// CDATA, comments, self-closing elements, prefixes and attributes.
    const CORPUS: &[&str] = &[
        "<SyncKey>5</SyncKey>",
        "<Sync><Collections><Collection><SyncKey>7</SyncKey><CollectionId>1</CollectionId></Collection></Collections></Sync>",
        "<A>x<A>y</A>z</A>",
        "<A>x<B>y</B>z</A>",
        "<A>R &amp; D</A>",
        "<A>R &#68; D</A>",
        "<A><![CDATA[a<b&c]]></A>",
        "<A>a<![CDATA[b]]>c</A>",
        "<A><!--c-->text</A>",
        "<A><B/>tail</A>",
        "<A/>",
        "<A></A>",
        "<A>  spaced  </A>",
        "<A>\n  multi\n  line\n</A>",
        "<AirSyncBase:Body><Data>abc</Data></AirSyncBase:Body>",
        "<a:A xmlns:a=\"x\">v</a:A>",
        "<A>unclosed",
        "<A>x</B>",
        "<A>1</A><A>2</A>",
        "<A><B>1</B></A><A>2</A>",
        "<A attr=\"v\">text</A>",
        "not xml at all",
        "",
    ];

    const TAGS: &[&[u8]] = &[b"A", b"B", b"SyncKey", b"CollectionId", b"Body", b"Data"];

    #[test]
    fn indexed_lookups_match_the_per_tag_helper() {
        for doc in CORPUS {
            let fields = RequestFields::parse(doc);
            for tag in TAGS {
                assert_eq!(
                    fields.first(tag),
                    first_tag_text(doc, tag),
                    "first({:?}) diverged on {doc:?}",
                    std::str::from_utf8(tag).unwrap()
                );
                let local = std::str::from_utf8(tag).unwrap();
                assert_eq!(
                    fields.first_outer(local),
                    first_outer_text(doc, local),
                    "first_outer({local:?}) diverged on {doc:?}"
                );
            }
        }
    }

    #[test]
    fn nested_same_name_takes_the_inner_value_but_outer_takes_the_outer() {
        let doc = "<A>x<A>y</A>z</A>";
        let fields = RequestFields::parse(doc);
        assert_eq!(fields.first(b"A").as_deref(), Some("y"));
        assert_eq!(fields.first_outer("A").as_deref(), Some("xyz"));
        // The reference helpers agree with the index, not with each other.
        assert_eq!(first_tag_text(doc, b"A").as_deref(), Some("y"));
        assert_eq!(first_outer_text(doc, "A").as_deref(), Some("xyz"));
    }

    #[test]
    fn reference_keeps_the_spaces_around_it() {
        let fields = RequestFields::parse("<A>R &amp; D</A>");
        assert_eq!(fields.first(b"A").as_deref(), Some("R & D"));
        // The outer rule does not trim, so the value is returned as-is.
        assert_eq!(
            RequestFields::parse("<A> R &amp; D </A>")
                .first_outer("A")
                .as_deref(),
            Some(" R & D ")
        );
    }

    #[test]
    fn self_closing_and_absent_elements_are_not_values() {
        let fields = RequestFields::parse("<A><B/></A>");
        assert_eq!(fields.first(b"B"), None);
        assert_eq!(fields.first_outer("B"), None);
        assert_eq!(fields.first(b"A").as_deref(), Some(""));
        assert_eq!(fields.first(b"Missing"), None);
    }

    #[test]
    fn prefixed_and_unprefixed_names_are_the_same_field() {
        let fields = RequestFields::parse("<AirSyncBase:Body><Data>abc</Data></AirSyncBase:Body>");
        assert_eq!(fields.first(b"Body").as_deref(), Some("abc"));
        assert_eq!(fields.first(b"Data").as_deref(), Some("abc"));
        assert_eq!(
            RequestFields::parse("<a:Body xmlns:a=\"x\">v</a:Body>")
                .first(b"Body")
                .as_deref(),
            Some("v")
        );
    }

    #[test]
    fn malformed_document_falls_back_to_the_helper() {
        for doc in ["<A>unclosed", "<A>x</B>", "not xml at all", ""] {
            let fields = RequestFields::parse(doc);
            for tag in TAGS {
                assert_eq!(
                    fields.first(tag),
                    first_tag_text(doc, tag),
                    "fallback diverged on {doc:?}"
                );
            }
        }
    }

    /// A pure-text element is answered from the body span, so a large value is
    /// never copied during the pass — only the value that is asked for.
    #[test]
    fn large_plain_element_is_materialised_only_when_read() {
        let blob = "A".repeat(2 * 1024 * 1024);
        let doc = format!(
            "<SendMail><ClientId>c1</ClientId><Mime>{blob}</Mime><SaveInSentItems/></SendMail>"
        );
        let fields = RequestFields::parse(&doc);
        assert_eq!(fields.first(b"ClientId").as_deref(), Some("c1"));
        assert_eq!(fields.first(b"Mime").map(|v| v.len()), Some(blob.len()));
        assert_eq!(fields.first(b"CollectionId"), None);
        assert_eq!(fields.first(b"ItemId"), None);
    }

    #[test]
    fn trim_owned_keeps_interior_whitespace() {
        assert_eq!(trim_owned("  a b  ".to_string()), "a b");
        assert_eq!(trim_owned("\n\ta\n".to_string()), "a");
        assert_eq!(trim_owned("   ".to_string()), "");
        assert_eq!(trim_owned(String::new()), "");
    }
}
