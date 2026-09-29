// src/eas_sync_options.rs
//
// EAS Sync collection option negotiation ([MS-ASCMD] §2.2.3.125.6 Options (Sync),
// [MS-ASAIRS] §2.2.2.12 BodyPreference, §2.2.2.11 BodyPartPreference).
//
// Outlook clients negotiate the shape of every Sync response through the
// per-collection `<Options>` block: which body format they want (plain text,
// HTML, RTF or MIME), how many bytes of it they accept (TruncationSize), whether
// a truncated body is acceptable at all (AllOrNone), whether they want a plain
// text Preview, and — for protocol version 14.1+ — the 16.1 conversation
// surfaces (`<BodyPartPreference>` message parts plus
// `<ConversationMode>`/`<Partial>`). The gateway previously ignored all of
// this and always returned the "native" body, which is wire-inexact against
// [MS-ASAIRS] §3.2.5.2.3 and breaks Outlook's message list (previews),
// conversation view and data-plan-sensitive truncation.

use quick_xml::Reader;
use quick_xml::events::Event;
use serde::{Deserialize, Serialize};

/// Collection-level Sync request flags that live outside `<Options>`
/// ([MS-ASCMD] §2.2.3.29.2): `<ConversationMode>` (§2.2.3.36.2, 16.1
/// conversation view) and `<DeletesAsMoves>` (§2.2.1.21.3). These are
/// per-request, not part of the sticky Options block.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CollectionLevelControls {
    /// `<airsync:ConversationMode>`: 1/absent-with-tag enables
    /// conversation-based filtering for this collection
    /// ([MS-ASCON] §2.2.2.5: "If this element is present without a value,
    /// the default value is 1").
    pub conversation_mode: Option<bool>,
    /// `<airsync:DeletesAsMoves>`: 1/absent-with-tag means client-side
    /// deletes are soft-deletes into the Deleted Items folder.
    pub deletes_as_moves: Option<bool>,
}

/// Result of parsing one `<Collection>` element's negotiation surface.
#[derive(Clone, Debug, Default)]
pub struct ParsedCollectionOptions {
    /// The `<Options>` block contents ([MS-ASCMD] §2.2.3.125.6).
    /// `explicitly_set` distinguishes "request carried `<Options>`" from
    /// "request had none" for the sticky-options rule.
    pub options: EasSyncCollectionOptions,
    /// Collection-level flags outside `<Options>`.
    pub controls: CollectionLevelControls,
}

/// Parse the `<Options>` block and collection-level control elements out of
/// one `<Collection>` element's XML.
///
/// The WBXML decoder emits qualified names for cross-code-page elements
/// (`<AirSyncBase:BodyPreference>`), while plain XML requests may use either
/// form; matching on the local name handles both. Text content is matched
/// case-insensitively for booleans per [MS-ASDTYPE] §2.1 ("1"/"0", with the
/// spec's default of TRUE for a present-but-valueless boolean tag handled by
/// the `Empty` event).
pub fn parse_collection_options(collection_xml: &str) -> ParsedCollectionOptions {
    let mut parsed = ParsedCollectionOptions::default();
    let mut options_seen = false;

    let mut reader = Reader::from_str(collection_xml);
    reader.config_mut().trim_text(true);

    // Stack of open element local names, e.g.
    // ["Collection", "Options", "BodyPreference", "Type"], with a parallel
    // flag per element recording whether it directly contained text. The
    // WBXML decoder renders valueless tags as `<Tag></Tag>` (Start+End, no
    // text event), so "opened, never got text" is how an empty tag presents
    // after decoding.
    let mut path: Vec<String> = Vec::new();
    let mut has_text: Vec<bool> = Vec::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let local = local_name(e.name().as_ref());
                // A new BodyPreference opens its own preference slot; the
                // child elements that follow fill it in request order.
                if path.len() == 2 && path[1] == "Options" && local == "BodyPreference" {
                    parsed.options.body_preferences.push(EasBodyPreference::default());
                }
                if path.len() == 1 && local == "Options" {
                    options_seen = true;
                }
                path.push(local);
                has_text.push(false);
            }
            Ok(Event::Empty(e)) => {
                // Self-closing tag: <Partial/>, <ConversationMode/> —
                // presence without a value defaults the boolean to TRUE.
                let local = local_name(e.name().as_ref());
                let mut empty_path = path.clone();
                empty_path.push(local.clone());
                assign_option_text(&mut parsed, &empty_path, &local, "");
            }
            Ok(Event::Text(t)) => {
                if let Some(seen) = has_text.last_mut() {
                    *seen = true;
                }
                if let Some(tag) = path.last() {
                    assign_option_text(&mut parsed, &path, tag, t.as_ref());
                }
            }
            Ok(Event::CData(t)) => {
                if let Some(seen) = has_text.last_mut() {
                    *seen = true;
                }
                if let Some(tag) = path.last() {
                    assign_option_text(&mut parsed, &path, tag, t.as_ref());
                }
            }
            Ok(Event::GeneralRef(r)) => {
                // Entity references inside option values decode through the
                // same shared table the rest of the EAS parser uses.
                if let Some(seen) = has_text.last_mut() {
                    *seen = true;
                }
                if let Some(tag) = path.last() {
                    let resolved = crate::util::resolve_xml_reference(r.as_ref());
                    assign_option_text(&mut parsed, &path, tag, &resolved);
                }
            }
            Ok(Event::End(_)) => {
                let local = path.pop();
                let saw_text = has_text.pop().unwrap_or(false);
                if let Some(tag) = local
                    && !saw_text
                    && is_valueless_boolean_tag(&tag)
                {
                    let mut empty_path = path.clone();
                    empty_path.push(tag.clone());
                    assign_option_text(&mut parsed, &empty_path, &tag, "");
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => {
                tracing::debug!("parse_collection_options: XML error: {e}");
                break;
            }
        }
        buf.clear();
    }

    parsed.options.explicitly_set = options_seen;
    parsed
}

/// Tags whose presence without a value defaults to TRUE per their defining
/// sections ([MS-ASCON] §2.2.2.5 ConversationMode, [MS-ASCMD] §2.2.1.21.3
/// DeletesAsMoves, [MS-ASAIRS] §2.2.2.3.1/§2.2.2.3.2 AllOrNone,
/// [MS-ASRM] §2.2.2.15 RightsManagementSupport).
fn is_valueless_boolean_tag(local: &str) -> bool {
    matches!(
        local,
        "ConversationMode" | "DeletesAsMoves" | "AllOrNone" | "RightsManagementSupport"
    )
}

/// Assign one element's text to the negotiation structures based on its path.
fn assign_option_text(
    parsed: &mut ParsedCollectionOptions,
    path: &[String],
    tag: &str,
    text: &str,
) {
    let text = text.trim();
    let options = &mut parsed.options;

    // Depth: ["Collection", "Options", ...] — direct children of Options.
    if path.len() == 3 && path[1] == "Options" {
        match tag {
            "FilterType" => options.filter_type = text.parse().ok(),
            "MIMESupport" => options.mime_support = text.parse().ok(),
            "MIMETruncation" => options.mime_truncation = text.parse().ok(),
            "MaxItems" => options.max_items = text.parse().ok(),
            "Truncation" => options.truncation = text.parse().ok(),
            "Conflict" => options.conflict = text.parse().ok(),
            "RightsManagementSupport" => {
                options.rights_management_support = text != "0";
            }
            _ => {}
        }
        return;
    }

    // Depth 4: ["Collection", "Options", "BodyPreference"|"BodyPartPreference", child].
    if path.len() == 4 && path[1] == "Options" {
        match path[2].as_str() {
            "BodyPreference" => {
                // The BodyPreference under construction is the last entry.
                let Some(pref) = options.body_preferences.last_mut() else {
                    return;
                };
                match tag {
                    "Type" => pref.body_type = text.parse().unwrap_or(1),
                    "TruncationSize" => pref.truncation_size = text.parse().ok(),
                    "AllOrNone" => pref.all_or_none = Some(text != "0"),
                    "Preview" => pref.preview = text.parse().ok(),
                    _ => {}
                }
            }
            "BodyPartPreference" => {
                let part = options
                    .body_part_preference
                    .get_or_insert_with(EasBodyPartPreference::default);
                match tag {
                    "Type" => part.body_type = text.parse().unwrap_or(2),
                    "TruncationSize" => part.truncation_size = text.parse().ok(),
                    "AllOrNone" => part.all_or_none = Some(text != "0"),
                    "Preview" => part.preview = text.parse().ok(),
                    _ => {}
                }
            }
            _ => {}
        }
        return;
    }

    // Collection-level direct children (["Collection", tag]).
    if path.len() == 2 && path[0] == "Collection" {
        match tag {
            "ConversationMode" => parsed.controls.conversation_mode = Some(text != "0"),
            "DeletesAsMoves" => parsed.controls.deletes_as_moves = Some(text != "0"),
            _ => {}
        }
    }
}

fn local_name(qualified: &str) -> String {
    match qualified.rfind(':') {
        Some(pos) => qualified[pos + 1..].to_string(),
        None => qualified.to_string(),
    }
}

/// One `<airsyncbase:BodyPreference>` ([MS-ASAIRS] §2.2.2.12).
///
/// A request MUST NOT contain more than one BodyPreference per allowable
/// `Type` value ([MS-ASAIRS] §2.2.2.12), and the child elements appear in the
/// fixed order Type, TruncationSize, AllOrNone, Preview.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EasBodyPreference {
    /// Body format: 1 = plain text, 2 = HTML, 3 = RTF, 4 = MIME
    /// ([MS-ASAIRS] §2.2.2.41.4). Required per BodyPreference.
    pub body_type: u8,
    /// Maximum content size in bytes ([MS-ASAIRS] §2.2.2.40.2). `None` = the
    /// entire content is used.
    pub truncation_size: Option<u32>,
    /// When `Some(true)` with a TruncationSize, the server MUST NOT return a
    /// truncated body of this type; it falls through to the next preference
    /// ([MS-ASAIRS] §2.2.2.3.2).
    pub all_or_none: Option<bool>,
    /// Requested plain-text preview length in Unicode characters, 0..=255
    /// ([MS-ASAIRS] §2.2.2.35.4).
    pub preview: Option<u8>,
}

/// One `<airsyncbase:BodyPartPreference>` ([MS-ASAIRS] §2.2.2.11), used by
/// protocol version 14.1+/16.1 conversation mode to request the *message part*
/// (the portion of an email that is original to it, without quoted history).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EasBodyPartPreference {
    /// Per [MS-ASCON] §3.2.5.7 only Type 2 (HTML) is meaningful for message
    /// parts; anything else is answered with status 164 on ItemOperations.
    pub body_type: u8,
    pub truncation_size: Option<u32>,
    pub all_or_none: Option<bool>,
    pub preview: Option<u8>,
}

/// The fully negotiated per-collection Sync options, i.e. everything the
/// server needs to shape `<ApplicationData>` for one collection.
///
/// Serialized as JSON for the "sticky options" persistence requirement of
/// [MS-ASCMD] §2.2.3.125.6: "The server preserves the Options block across
/// requests … If the Options block is not included in a request, the previous
/// Options block is used. Whenever the client specifies new options … the
/// server MUST replace the original Options block with the new Options block."
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EasSyncCollectionOptions {
    /// `<airsync:FilterType>` ([MS-ASCMD] §2.2.3.68.2): 0=all, 1=1 day,
    /// 2=3 days, 3=1 week, 4=2 weeks, 5=1 month, 6=3 months, 7=6 months.
    pub filter_type: Option<u8>,
    /// `<airsyncbase:BodyPreference>` elements in request order (the order
    /// defines the client's preference chain).
    pub body_preferences: Vec<EasBodyPreference>,
    /// `<airsyncbase:BodyPartPreference>` (14.1+/16.1 conversation parts).
    pub body_part_preference: Option<EasBodyPartPreference>,
    /// `<airsync:MIMESupport>` ([MS-ASCMD] §2.2.3.110.3): 0 = never send
    /// MIME, 1 = S/MIME messages only, 2 = all messages.
    pub mime_support: Option<u8>,
    /// `<airsync:MIMETruncation>` ([MS-ASCMD] §2.2.3.111): 0..8 thresholds,
    /// 8 = never truncate MIME data.
    pub mime_truncation: Option<u8>,
    /// `<airsync:MaxItems>` ([MS-ASCMD] §2.2.3.103.2).
    pub max_items: Option<u32>,
    /// `<airsync:Truncation>` ([MS-ASCMD] §2.2.3.185); EAS 2.5 only, kept for
    /// wire completeness.
    pub truncation: Option<u32>,
    /// `<airsync:Conflict>` ([MS-ASCMD] §2.2.3.34): 0 = server wins,
    /// 1 = client wins.
    pub conflict: Option<u8>,
    /// `<rm:RightsManagementSupport>` ([MS-ASRM] §2.2.2.15).
    pub rights_management_support: bool,
    /// Marks that the request actually carried an `<Options>` block —
    /// distinguishes "request had `<Options>`" from "request had none" for
    /// the sticky-options rule ([MS-ASCMD] §2.2.3.125.6). `serde(skip)` keeps
    /// it out of the persisted sticky block: a stored block is never
    /// "incoming", and deserialization restores the parser default (false).
    #[serde(skip)]
    pub explicitly_set: bool,
}

impl EasSyncCollectionOptions {
    /// Whether this options block contains any real preference at all.
    pub fn is_meaningful(&self) -> bool {
        self.filter_type.is_some()
            || !self.body_preferences.is_empty()
            || self.body_part_preference.is_some()
            || self.mime_support.is_some()
            || self.mime_truncation.is_some()
            || self.max_items.is_some()
            || self.truncation.is_some()
            || self.conflict.is_some()
            || self.rights_management_support
    }

    /// Merge two options blocks per the sticky-options rule of
    /// [MS-ASCMD] §2.2.3.125.6: a request whose collection carries an
    /// `<Options>` block replaces the stored block wholesale; a request
    /// without one reuses the previously stored block.
    pub fn resolve_sticky(current: Option<Self>, incoming: Option<Self>) -> Option<Self> {
        match incoming {
            Some(opts) if opts.explicitly_set => Some(opts),
            _ => current,
        }
    }

    /// Effective MIMESupport value; absent means 0
    /// ([MS-ASCMD] §2.2.3.110.3).
    pub fn effective_mime_support(&self) -> u8 {
        self.mime_support.unwrap_or(0)
    }
}

/// The body format the server selected for one item after negotiation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NegotiatedBody {
    /// 1 = plain text, 2 = HTML, 3 = RTF, 4 = MIME
    /// ([MS-ASAIRS] §2.2.2.41.1).
    pub body_type: u8,
    /// Byte limit from the selected preference, if any.
    pub truncation_size: Option<u32>,
    /// Preview length in Unicode characters, if the selected preference (or
    /// the BodyPartPreference, which is honored separately) asked for one.
    pub preview: Option<u8>,
}

/// The native storage format of an item's body, which drives preference
/// selection ([MS-ASAIRS] §2.2.2.3.2: "the server returns the data truncated to
/// the size requested by TruncationSize for the Type element that matches the
/// native storage format of the item's Body element").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeBodyType {
    PlainText,
    Html,
    Rtf,
    Mime,
}

impl NativeBodyType {
    /// The [MS-ASAIRS] §2.2.2.41.1 `Type` numeric value.
    pub fn wire_value(self) -> u8 {
        match self {
            NativeBodyType::PlainText => 1,
            NativeBodyType::Html => 2,
            NativeBodyType::Rtf => 3,
            NativeBodyType::Mime => 4,
        }
    }
}

/// Negotiate the body to return for one item
/// ([MS-ASAIRS] §2.2.2.12, §2.2.2.3.2, §2.2.2.40.2).
///
/// Selection rules, in order:
/// 1. Walk the client's BodyPreference chain in request order. The first
///    preference whose `Type` matches the item's native format wins, subject
///    to `AllOrNone`: if `AllOrNone` is TRUE and the available content exceeds
///    `TruncationSize`, this preference is skipped and the server "selects
///    the next BodyPreference element that will return the maximum amount of
///    body text to the client" ([MS-ASAIRS] §2.2.2.3.2).
/// 2. If no preference matches the native format, the first convertible
///    preference wins: plain text and HTML are always interconvertible (the
///    spec example converts an HTML-native body to plain text for the
///    client), RTF/MIME preferences are only satisfied when the native format
///    already is RTF/MIME (or MIMESupport allows a MIME rendition).
/// 3. With no preferences at all, return the native format untruncated —
///    the pre-16.1 behavior for clients that never negotiate.
pub fn negotiate_body(
    options: &EasSyncCollectionOptions,
    native: NativeBodyType,
    native_size_bytes: usize,
) -> NegotiatedBody {
    let preferences = &options.body_preferences;
    if preferences.is_empty() {
        return NegotiatedBody {
            body_type: native.wire_value(),
            truncation_size: None,
            preview: None,
        };
    }

    // Pass 1: exact native-format match, honoring AllOrNone.
    for pref in preferences {
        if pref.body_type == native.wire_value() && !skipped_by_all_or_none(pref, native_size_bytes) {
            return NegotiatedBody {
                body_type: pref.body_type,
                truncation_size: pref.truncation_size,
                preview: pref.preview,
            };
        }
    }

    // Pass 2: first convertible preference, still honoring AllOrNone.
    for pref in preferences {
        if pref.body_type != native.wire_value()
            && is_convertible(pref.body_type, native, options.effective_mime_support())
            && !skipped_by_all_or_none(pref, native_size_bytes)
        {
            return NegotiatedBody {
                body_type: pref.body_type,
                truncation_size: pref.truncation_size,
                preview: pref.preview,
            };
        }
    }

    // Pass 3: nothing satisfied the client's constraints. [MS-ASAIRS]
    // §2.2.2.12 does not define an error path for an unsatisfiable chain, and
    // Sync status 6 would force the client to drop the item entirely. Return
    // the native format with the first requested truncation so the item still
    // syncs; the `Truncated` flag tells the client what happened.
    let first = &preferences[0];
    NegotiatedBody {
        body_type: native.wire_value(),
        truncation_size: first.truncation_size,
        preview: first.preview,
    }
}

/// AllOrNone skip test ([MS-ASAIRS] §2.2.2.3.2): the element is ignored when
/// TruncationSize is absent.
fn skipped_by_all_or_none(pref: &EasBodyPreference, native_size_bytes: usize) -> bool {
    match (pref.all_or_none, pref.truncation_size) {
        (Some(true), Some(limit)) => native_size_bytes > limit as usize,
        _ => false,
    }
}

/// Whether `wanted` can be produced from `native`.
///
/// Plain text and HTML are interconvertible (server-side conversion is the
/// documented behavior in the [MS-ASAIRS] §2.2.2.3.2 example). RTF is never
/// synthesized. MIME is only produced when the client asked for MIME bodies
/// via MIMESupport ([MS-ASCMD] §2.2.3.110.3).
fn is_convertible(wanted: u8, native: NativeBodyType, mime_support: u8) -> bool {
    match wanted {
        1 | 2 => matches!(native, NativeBodyType::PlainText | NativeBodyType::Html),
        3 => native == NativeBodyType::Rtf,
        4 => mime_support != 0 || native == NativeBodyType::Mime,
        _ => false,
    }
}

/// Byte-boundary-safe truncation of UTF-8 content to at most `limit` bytes
/// ([MS-ASAIRS] §2.2.2.40.2 sizes are in bytes).
///
/// Returns the truncated slice and whether truncation occurred. Slicing never
/// splits a UTF-8 code point.
pub fn truncate_utf8_bytes(content: &str, limit: u32) -> (&str, bool) {
    if content.len() <= limit as usize {
        return (content, false);
    }
    let mut end = limit as usize;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    (&content[..end], true)
}

/// Extract up to `max_chars` Unicode characters of plain text for the
/// `<AirSyncBase:Preview>` element ([MS-ASAIRS] §2.2.2.35.1: "MUST contain no
/// more than the number of characters specified in the request").
pub fn preview_text(plain: &str, max_chars: u8) -> String {
    plain.chars().take(max_chars as usize).collect()
}

/// Strip HTML markup to a plain-text approximation for Type 1 bodies and
/// previews. Uses the existing dependency-free approach: tags are removed,
/// entities for the five XML-mandatory characters are decoded, block-level
/// tags introduce line breaks, and consecutive whitespace is collapsed.
pub fn html_to_plain_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let bytes = html.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'<' => {
                if let Some(close) = html[i..].find('>') {
                    let tag = &html[i + 1..i + close];
                    let lower = tag.to_ascii_lowercase();
                    let is_block = lower.starts_with("p")
                        || lower.starts_with("div")
                        || lower.starts_with("br")
                        || lower.starts_with("li")
                        || lower.starts_with("tr")
                        || lower.starts_with("/p")
                        || lower.starts_with("/div")
                        || lower.starts_with("/li")
                        || lower.starts_with("/tr")
                        || lower.starts_with("h1")
                        || lower.starts_with("h2")
                        || lower.starts_with("h3")
                        || lower.starts_with("h4")
                        || lower.starts_with("/h1")
                        || lower.starts_with("/h2")
                        || lower.starts_with("/h3")
                        || lower.starts_with("/h4");
                    if is_block {
                        out.push('\n');
                    }
                    i += close + 1;
                } else {
                    out.push('<');
                    i += 1;
                }
            }
            b'&' => {
                // Decode the five predefined XML entities ([XML1.0] §4.6) and
                // numeric character references. The entity tails are matched
                // separately from the leading ampersand.
                let rest = &html[i..];
                let decoded = if rest.starts_with('&')
                    && let Some(semi) = rest.find(';')
                    && semi > 1
                {
                    match &rest[1..semi] {
                        "amp" => Some('&'),
                        "lt" => Some('<'),
                        "gt" => Some('>'),
                        "quot" => Some('"'),
                        "apos" => Some('\''),
                        digits if digits.starts_with('#') => {
                            let code = if let Some(hex) = digits
                                .strip_prefix("#x")
                                .or_else(|| digits.strip_prefix("#X"))
                            {
                                u32::from_str_radix(hex, 16).ok()
                            } else {
                                digits.strip_prefix('#').and_then(|d| d.parse().ok())
                            };
                            code.and_then(char::from_u32)
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                match decoded {
                    Some(ch) => {
                        out.push(ch);
                        i += rest.find(';').unwrap_or(0) + 1;
                    }
                    None => {
                        out.push('&');
                        i += 1;
                    }
                }
            }
            other => {
                // Copy the full UTF-8 code point.
                let ch_len = utf8_len(other);
                let end = (i + ch_len).min(bytes.len());
                out.push_str(&html[i..end]);
                i = end;
            }
        }
    }
    out
}

fn utf8_len(first_byte: u8) -> usize {
    match first_byte {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pref(body_type: u8) -> EasBodyPreference {
        EasBodyPreference {
            body_type,
            ..Default::default()
        }
    }

    #[test]
    fn no_preferences_returns_native_untruncated() {
        let opts = EasSyncCollectionOptions::default();
        let body = negotiate_body(&opts, NativeBodyType::Html, 10_000);
        assert_eq!(body.body_type, 2);
        assert_eq!(body.truncation_size, None);
    }

    #[test]
    fn native_type_match_wins_in_order() {
        let opts = EasSyncCollectionOptions {
            body_preferences: vec![
                EasBodyPreference {
                    body_type: 2,
                    truncation_size: Some(20_480),
                    ..Default::default()
                },
                EasBodyPreference {
                    body_type: 1,
                    truncation_size: Some(512),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        // Plain-native item: first pref (HTML) is convertible, second matches.
        let body = negotiate_body(&opts, NativeBodyType::PlainText, 100);
        // Pass 1 finds the Type 1 preference (native match) even though the
        // Type 2 preference comes first in request order.
        assert_eq!(body.body_type, 1);
        assert_eq!(body.truncation_size, Some(512));
    }

    #[test]
    fn all_or_none_skips_oversized_preference() {
        // The [MS-ASAIRS] §2.2.2.3.2 example: an HTML-native item >50 bytes
        // must fall through the AllOrNone HTML pref to the plain-text pref.
        let opts = EasSyncCollectionOptions {
            body_preferences: vec![
                EasBodyPreference {
                    body_type: 2,
                    truncation_size: Some(50),
                    all_or_none: Some(true),
                    ..Default::default()
                },
                EasBodyPreference {
                    body_type: 1,
                    truncation_size: Some(50),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let body = negotiate_body(&opts, NativeBodyType::Html, 200);
        assert_eq!(body.body_type, 1);
        assert_eq!(body.truncation_size, Some(50));
    }

    #[test]
    fn all_or_none_without_truncation_size_is_ignored() {
        let opts = EasSyncCollectionOptions {
            body_preferences: vec![EasBodyPreference {
                body_type: 2,
                truncation_size: Some(50),
                all_or_none: Some(true),
                ..Default::default()
            }],
            ..Default::default()
        };
        let body = negotiate_body(&opts, NativeBodyType::Html, 200);
        assert_eq!(body.body_type, 2);
    }

    #[test]
    fn html_native_converts_to_plain_when_only_plain_requested() {
        let opts = EasSyncCollectionOptions {
            body_preferences: vec![pref(1)],
            ..Default::default()
        };
        let body = negotiate_body(&opts, NativeBodyType::Html, 5_000);
        assert_eq!(body.body_type, 1);
    }

    #[test]
    fn rtf_is_never_synthesized() {
        let opts = EasSyncCollectionOptions {
            body_preferences: vec![pref(3), pref(2)],
            ..Default::default()
        };
        let body = negotiate_body(&opts, NativeBodyType::Html, 100);
        assert_eq!(body.body_type, 2);
    }

    #[test]
    fn mime_preference_requires_mime_support() {
        let mut opts = EasSyncCollectionOptions {
            body_preferences: vec![pref(4), pref(1)],
            ..Default::default()
        };
        let body = negotiate_body(&opts, NativeBodyType::Html, 100);
        assert_eq!(body.body_type, 1);
        opts.mime_support = Some(2);
        let body = negotiate_body(&opts, NativeBodyType::Html, 100);
        assert_eq!(body.body_type, 4);
    }

    #[test]
    fn truncate_utf8_bytes_never_splits_code_points() {
        let content = "héllo wörld"; // 11 chars, 13 bytes
        let (cut, truncated) = truncate_utf8_bytes(content, 2);
        assert_eq!(cut, "h");
        assert!(truncated);
        let (cut, truncated) = truncate_utf8_bytes(content, 2_000);
        assert_eq!(cut, content);
        assert!(!truncated);
    }

    #[test]
    fn preview_limits_characters() {
        assert_eq!(preview_text("abcdefghij", 4), "abcd");
        assert_eq!(preview_text("日本語テキスト", 3), "日本語");
        assert_eq!(preview_text("short", 255), "short");
    }

    #[test]
    fn html_to_plain_text_decodes_entities_and_breaks_blocks() {
        let html = "<div>Hello & welcome</div><p>Line2</p><br/>Tail";
        let plain = html_to_plain_text(html);
        assert!(plain.contains("Hello & welcome"));
        assert!(plain.contains("Line2"));
        assert!(plain.contains("Tail"));
        assert!(plain.contains('\n'));
    }

    #[test]
    fn sticky_options_resolution() {
        let old = EasSyncCollectionOptions {
            filter_type: Some(3),
            ..Default::default()
        };
        // No Options in the new request -> previous block preserved.
        assert_eq!(
            EasSyncCollectionOptions::resolve_sticky(Some(old.clone()), None),
            Some(old.clone())
        );
        // A parsed block from a request without <Options> also preserves
        // the previous block (explicitly_set is false).
        assert_eq!(
            EasSyncCollectionOptions::resolve_sticky(Some(old.clone()), Some(Default::default())),
            Some(old.clone())
        );
        // New Options block -> replace wholesale.
        let next = EasSyncCollectionOptions {
            body_preferences: vec![pref(2)],
            explicitly_set: true,
            ..Default::default()
        };
        assert_eq!(
            EasSyncCollectionOptions::resolve_sticky(Some(old), Some(next.clone())),
            Some(next)
        );
    }

    #[test]
    fn parse_outlook_android_option_matrix() {
        // The exact Options matrix Outlook for Android (5.2634.2) sends for
        // the Inbox on a 16.1 prime: date filter, HTML body preference with a
        // 20 KiB truncation, plain-text fallback, and a 255-char preview.
        let xml = r#"<Collection><Class>Email</Class><SyncKey>0</SyncKey><CollectionId>2</CollectionId>
            <Options>
              <FilterType>1</FilterType>
              <BodyPreference>
                <Type>2</Type>
                <TruncationSize>20480</TruncationSize>
                <AllOrNone>0</AllOrNone>
                <Preview>255</Preview>
              </BodyPreference>
              <BodyPreference>
                <Type>1</Type>
                <TruncationSize>512</TruncationSize>
                <AllOrNone>0</AllOrNone>
                <Preview>255</Preview>
              </BodyPreference>
              <MIMESupport>2</MIMESupport>
              <MIMETruncation>8</MIMETruncation>
              <MaxItems>50</MaxItems>
              <ConversationMode>1</ConversationMode>
            </Options>
          </Collection>"#;
        let parsed = parse_collection_options(xml);
        assert!(parsed.options.explicitly_set);
        assert_eq!(parsed.options.filter_type, Some(1));
        assert_eq!(parsed.options.mime_support, Some(2));
        assert_eq!(parsed.options.mime_truncation, Some(8));
        assert_eq!(parsed.options.max_items, Some(50));
        assert_eq!(parsed.options.body_preferences.len(), 2);
        assert_eq!(
            parsed.options.body_preferences[0],
            EasBodyPreference {
                body_type: 2,
                truncation_size: Some(20480),
                all_or_none: Some(false),
                preview: Some(255),
            }
        );
        assert_eq!(
            parsed.options.body_preferences[1],
            EasBodyPreference {
                body_type: 1,
                truncation_size: Some(512),
                all_or_none: Some(false),
                preview: Some(255),
            }
        );
        // ConversationMode in this sample sits inside Options (some client
        // builds nest it there); the collection-level position is exercised
        // in parse_new_outlook_windows_option_matrix.
    }

    #[test]
    fn parse_new_outlook_windows_option_matrix() {
        // New Outlook for Windows (20251205004.10) Sync prime: 16.1
        // conversation mode with Partial, collection-level ConversationMode
        // (its spec position, [MS-ASCMD] §2.2.3.36.2) and BodyPartPreference
        // for message parts.
        let xml = r#"<Collection><Class>Email</Class><SyncKey>0</SyncKey><CollectionId>2</CollectionId>
            <ConversationMode>1</ConversationMode>
            <WindowSize>100</WindowSize>
            <Options>
              <FilterType>4</FilterType>
              <BodyPreference>
                <Type>2</Type>
                <TruncationSize>51200</TruncationSize>
              </BodyPreference>
              <BodyPartPreference>
                <Type>2</Type>
                <TruncationSize>4096</TruncationSize>
                <Preview>255</Preview>
              </BodyPartPreference>
              <RightsManagementSupport>1</RightsManagementSupport>
            </Options>
          </Collection>"#;
        let parsed = parse_collection_options(xml);
        assert!(parsed.options.explicitly_set);
        assert_eq!(parsed.options.filter_type, Some(4));
        assert_eq!(parsed.controls.conversation_mode, Some(true));
        assert_eq!(
            parsed.options.body_part_preference,
            Some(EasBodyPartPreference {
                body_type: 2,
                truncation_size: Some(4096),
                all_or_none: None,
                preview: Some(255),
            })
        );
        assert!(parsed.options.rights_management_support);
        assert_eq!(parsed.options.body_preferences.len(), 1);
    }

    #[test]
    fn parse_wbxml_qualified_names() {
        // After WBXML decode, cross-code-page elements carry their prefix
        // ([MS-ASWBXML] §2.1.2.1.18: AirSyncBase:BodyPreference 0x05).
        let xml = r#"<Collection><SyncKey>0</SyncKey><CollectionId>2</CollectionId><Options><AirSyncBase:BodyPreference><AirSyncBase:Type>1</AirSyncBase:Type><AirSyncBase:TruncationSize>1024</AirSyncBase:TruncationSize></AirSyncBase:BodyPreference></Options></Collection>"#;
        let parsed = parse_collection_options(xml);
        assert_eq!(
            parsed.options.body_preferences,
            vec![EasBodyPreference {
                body_type: 1,
                truncation_size: Some(1024),
                all_or_none: None,
                preview: None,
            }]
        );
    }

    #[test]
    fn parse_empty_boolean_tags_default_true() {
        // WBXML-decoded valueless tags appear as <Tag></Tag>; Quick-XML Empty
        // events appear as <Tag/>. Both mean TRUE for the boolean flags.
        let xml = r#"<Collection><ConversationMode></ConversationMode><DeletesAsMoves/><Options><BodyPreference><Type>2</Type><AllOrNone></AllOrNone></BodyPreference></Options></Collection>"#;
        let parsed = parse_collection_options(xml);
        assert_eq!(parsed.controls.conversation_mode, Some(true));
        assert_eq!(parsed.controls.deletes_as_moves, Some(true));
        assert_eq!(parsed.options.body_preferences[0].all_or_none, Some(true));
    }

    #[test]
    fn parse_absent_options_marks_not_explicit() {
        let xml = r#"<Collection><Class>Email</Class><SyncKey>abc</SyncKey><CollectionId>2</CollectionId></Collection>"#;
        let parsed = parse_collection_options(xml);
        assert!(!parsed.options.explicitly_set);
        assert!(!parsed.options.is_meaningful());
    }

    #[test]
    fn parse_ignores_mutation_payloads() {
        // Client mutation ApplicationData (e.g. an outgoing email body)
        // must never be mistaken for negotiation options.
        let xml = r#"<Collection><SyncKey>k</SyncKey><CollectionId>2</CollectionId><Commands><Add><ClientId>1</ClientId><ApplicationData><AirSyncBase:Body><AirSyncBase:Type>1</AirSyncBase:Type><AirSyncBase:Data>ConversationMode text</AirSyncBase:Data></AirSyncBase:Body></ApplicationData></Add></Commands></Collection>"#;
        let parsed = parse_collection_options(xml);
        assert!(!parsed.options.explicitly_set);
        assert_eq!(parsed.controls.conversation_mode, None);
        assert!(parsed.options.body_preferences.is_empty());
    }

    #[test]
    fn effective_mime_support_defaults_to_zero() {
        assert_eq!(EasSyncCollectionOptions::default().effective_mime_support(), 0);
        let opts = EasSyncCollectionOptions {
            mime_support: Some(2),
            ..Default::default()
        };
        assert_eq!(opts.effective_mime_support(), 2);
    }
}
