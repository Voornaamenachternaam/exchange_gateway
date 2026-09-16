// src/mapi/rules.rs
//
// MS-OXORULE inbox-rule persistence + execution, bridged onto Stalwart's
// JMAP Sieve capability (RFC 9661 `SieveScript/get`/`set`).
//
// Storage model
// -------------
// MAPI rules are persisted inside the user's active Sieve script as a
// self-delimiting comment block carrying the CANONICAL rule set as
// hex-encoded JSON, followed by the executable Sieve translation of every
// rule the Sieve model can express faithfully:
//
// ```text
// ... other user/OOF script content ...
// # MAPI-RULES-BEGIN v1
// # <hex(json(rules))>
// # MAPI-RULES-GENERATED
// if allof(header :contains "From" "a@b") { fileinto "M123"; stop; }
// # MAPI-RULES-END
// ```
//
// * The JSON block is the source of truth for the MAPI surface: the
//   restriction and action bytes are stored verbatim (hex) so every
//   property — including client-opaque ones like `PR_RULE_USER_FLAGS` /
//   `PR_RULE_PROVIDER_DATA` — round-trips byte-exactly through
//   `RopModifyRules` → `RopGetRulesTable`.
// * The generated Sieve lines are derived materialisation. Rules whose
//   condition or actions cannot be expressed in Sieve are stored (and
//   reported back to the client with `ST_ERROR` set, as MS-OXORULE §2.2.1.3.1.3
//   prescribes for rules the server fails to process) but emit only a
//   comment — they never silently fire with degraded semantics.
// * `render_script` re-anchors the block at the end of the script and
//   preserves all non-rule content (notably the OOF `vacation` lines the
//   OOF manager writes), so the two writers never clobber each other.

use crate::mapi::data::{PropertyTag, PropertyType, PropertyValue, TaggedPropertyValue};
use crate::mapi::restrict::SRestriction;
use crate::mapi::rops::{Buf, DecodeError};

// ---- rule property tags (MS-OXORULE §2.2.1.3.1) ---------------------------

pub const PR_RULE_ID: u16 = 0x6674; // PtypInteger64
pub const PR_RULE_STATE: u16 = 0x6677; // PtypInteger32
pub const PR_RULE_SEQUENCE: u16 = 0x6676;
pub const PR_RULE_USER_FLAGS: u16 = 0x6678;
pub const PR_RULE_CONDITION: u16 = 0x6679; // PtypRestriction
pub const PR_RULE_ACTIONS: u16 = 0x6680; // PtypRuleAction
pub const PR_RULE_PROVIDER: u16 = 0x6681; // PtypString
pub const PR_RULE_NAME: u16 = 0x6682; // PtypString
pub const PR_RULE_LEVEL: u16 = 0x6683; // PtypInteger32
pub const PR_RULE_PROVIDER_DATA: u16 = 0x6684; // PtypBinary

pub const ST_ENABLED: u32 = 0x0000_0001;
pub const ST_ERROR: u32 = 0x0000_0002;
pub const ST_ONLY_WHEN_OOF: u32 = 0x0000_0004;
pub const ST_KEEP_OOF_HIST: u32 = 0x0000_0008;
pub const ST_EXIT_LEVEL: u32 = 0x0000_0010;

pub const RULE_ROW_ADD: u8 = 0x01;
pub const RULE_ROW_MODIFY: u8 = 0x02;
pub const RULE_ROW_REMOVE: u8 = 0x04;

const BLOCK_BEGIN: &str = "# MAPI-RULES-BEGIN v1";
const BLOCK_GENERATED: &str = "# MAPI-RULES-GENERATED";
const BLOCK_END: &str = "# MAPI-RULES-END";

/// The provider string the gateway reports for rules it generated itself.
pub const GATEWAY_RULE_PROVIDER: &str = "exchange_gateway";

/// A persisted MAPI rule. `condition` holds the exact `PtypRestriction`
/// bytes the client sent (MS-OXCDATA §2.12 `SRestriction` serialisation);
/// `actions` holds the exact `PtypRuleAction` bytes (16-bit COUNT prefix +
/// that many ActionBlocks, MS-OXORULE §2.2.5.1). Keeping the raw bytes
// makes the table read-back byte-identical and lets the Sieve generator
// re-derive the translation deterministically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRule {
    pub id: u64,
    pub name: String,
    pub sequence: u32,
    pub state: u32,
    pub level: u32,
    pub user_flags: u32,
    pub provider: String,
    pub provider_data: Vec<u8>,
    pub condition: Vec<u8>,
    pub actions: Vec<u8>,
}

// ---- hex helpers (script comments are single-line) -------------------------

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
        s.push(char::from_digit(u32::from(b & 0xF), 16).unwrap_or('0'));
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if !b.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(b.len() / 2);
    for pair in b.as_chunks::<2>().0 {
        let hi = (pair[0] as char).to_digit(16)? as u8;
        let lo = (pair[1] as char).to_digit(16)? as u8;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

// ---- rule <-> property set conversion --------------------------------------

fn get_tag(props: &[TaggedPropertyValue], id: u16) -> Option<&PropertyValue> {
    props
        .iter()
        .find(|p| p.tag.property_id == id)
        .map(|p| &p.value)
}

fn as_i32(v: Option<&PropertyValue>) -> i32 {
    match v {
        Some(PropertyValue::Integer32(n)) => *n,
        Some(PropertyValue::Integer64(n)) => i32::try_from(*n).unwrap_or(0),
        _ => 0,
    }
}

fn as_string(v: Option<&PropertyValue>) -> String {
    match v {
        Some(PropertyValue::String(s)) | Some(PropertyValue::String8(s)) => s.clone(),
        _ => String::new(),
    }
}

fn as_binary(v: Option<&PropertyValue>) -> Vec<u8> {
    match v {
        Some(PropertyValue::Binary(b)) => b.clone(),
        Some(PropertyValue::Opaque { bytes, .. }) => bytes.clone(),
        _ => Vec::new(),
    }
}

/// Build a `StoredRule` from one `RuleData` entry (MS-OXCROPS §2.2.11.1.1.1).
/// `flags` is the RuleDataFlags byte; `existing` is the rule being modified
/// when `flags` carries `RULE_ROW_MODIFY` (used to keep the id).
pub fn rule_from_rule_data(
    flags: u8,
    props: &[TaggedPropertyValue],
    existing: Option<&StoredRule>,
) -> StoredRule {
    let id = match get_tag(props, PR_RULE_ID) {
        Some(PropertyValue::Integer64(n)) if flags & RULE_ROW_ADD == 0 => *n as u64,
        _ => existing.map(|r| r.id).unwrap_or(0),
    };
    // Condition/action bytes: the ROP decoder stores the exact wire span as
    // `Opaque` for the PTYP_RESTRICTION/PTYP_RULE_ACTION types.
    let condition = as_binary(get_tag(props, PR_RULE_CONDITION));
    let actions = as_binary(get_tag(props, PR_RULE_ACTIONS));
    StoredRule {
        id,
        name: as_string(get_tag(props, PR_RULE_NAME)),
        sequence: as_i32(get_tag(props, PR_RULE_SEQUENCE)) as u32,
        // ST_ERROR is server-owned (§2.2.1.3.1.3): ignore client-supplied
        // state here; the render pass re-derives it.
        state: as_i32(get_tag(props, PR_RULE_STATE)) as u32 & !ST_ERROR,
        level: as_i32(get_tag(props, PR_RULE_LEVEL)) as u32,
        user_flags: as_i32(get_tag(props, PR_RULE_USER_FLAGS)) as u32,
        provider: as_string(get_tag(props, PR_RULE_PROVIDER)),
        provider_data: as_binary(get_tag(props, PR_RULE_PROVIDER_DATA)),
        condition,
        actions,
    }
}

/// The inverse of [`rule_from_rule_data`]: the property list a rules-table
/// row carries for this rule, for whichever column subset the client asked.
pub fn rule_to_cells(rule: &StoredRule, tags: &[PropertyTag]) -> Vec<PropertyValue> {
    tags.iter()
        .map(|t| {
            let id = t.property_id;
            match id {
                PR_RULE_ID => PropertyValue::Integer64(rule.id as i64),
                PR_RULE_NAME => PropertyValue::String(rule.name.clone()),
                PR_RULE_SEQUENCE => PropertyValue::Integer32(rule.sequence as i32),
                PR_RULE_STATE => PropertyValue::Integer32(rule.state as i32),
                PR_RULE_LEVEL => PropertyValue::Integer32(rule.level as i32),
                PR_RULE_USER_FLAGS => PropertyValue::Integer32(rule.user_flags as i32),
                PR_RULE_PROVIDER => PropertyValue::String(rule.provider.clone()),
                PR_RULE_PROVIDER_DATA => PropertyValue::Binary(rule.provider_data.clone()),
                PR_RULE_CONDITION => PropertyValue::Opaque {
                    property_type: PropertyType::PTYP_RESTRICTION,
                    bytes: rule.condition.clone(),
                },
                PR_RULE_ACTIONS => PropertyValue::Opaque {
                    property_type: PropertyType::PTYP_RULE_ACTION,
                    bytes: rule.actions.clone(),
                },
                _ => PropertyValue::Null,
            }
        })
        .collect()
}

// ---- script block parse / strip / render ------------------------------------

/// Serialise the rule list to its JSON-in-comment form.
fn rules_to_block(rules: &[StoredRule]) -> Vec<String> {
    let arr: Vec<serde_json::Value> = rules
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "name": r.name,
                "sequence": r.sequence,
                "state": r.state,
                "level": r.level,
                "user_flags": r.user_flags,
                "provider": r.provider,
                "provider_data": hex_encode(&r.provider_data),
                "condition": hex_encode(&r.condition),
                "actions": hex_encode(&r.actions),
            })
        })
        .collect();
    let json = serde_json::to_string(&arr).unwrap_or_else(|_| "[]".to_string());
    vec![
        BLOCK_BEGIN.to_string(),
        format!("# {}", hex_encode(json.as_bytes())),
    ]
}

/// Parse the stored rule list out of a Sieve script. Scripts without a
/// gateway block yield an empty set; a malformed block is reported as
/// empty but logged so a rules-table read degrades to "no rules" instead
/// of leaking a corrupt script to the client.
pub fn parse_rules(script: &str) -> Vec<StoredRule> {
    let mut in_block = false;
    for line in script.lines() {
        let line = line.trim_end();
        if line == BLOCK_BEGIN {
            in_block = true;
            continue;
        }
        if line == BLOCK_GENERATED || line == BLOCK_END {
            break;
        }
        if in_block && let Some(hex) = line.strip_prefix("# ") {
            let Some(json_bytes) = hex_decode(hex) else {
                tracing::warn!("MAPI rules block: invalid hex payload");
                return Vec::new();
            };
            let Ok(json) = serde_json::from_slice::<serde_json::Value>(&json_bytes) else {
                tracing::warn!("MAPI rules block: invalid JSON payload");
                return Vec::new();
            };
            let Some(items) = json.as_array() else {
                return Vec::new();
            };
            let mut rules = Vec::with_capacity(items.len());
            for it in items {
                let s = |k: &str| it.get(k).and_then(|v| v.as_str()).unwrap_or("");
                let n = |k: &str| it.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
                rules.push(StoredRule {
                    id: n("id"),
                    name: s("name").to_string(),
                    sequence: u32::try_from(n("sequence")).unwrap_or(0),
                    state: u32::try_from(n("state")).unwrap_or(0),
                    level: u32::try_from(n("level")).unwrap_or(0),
                    user_flags: u32::try_from(n("user_flags")).unwrap_or(0),
                    provider: s("provider").to_string(),
                    provider_data: hex_decode(s("provider_data")).unwrap_or_default(),
                    condition: hex_decode(s("condition")).unwrap_or_default(),
                    actions: hex_decode(s("actions")).unwrap_or_default(),
                });
            }
            return rules;
        }
    }
    Vec::new()
}

/// The script with any existing gateway rules block (and its generated
/// section) removed. Anything outside the markers is preserved verbatim.
pub fn strip_rules_block(script: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut in_block = false;
    for line in script.lines() {
        let t = line.trim_end();
        if t == BLOCK_BEGIN {
            in_block = true;
            // Drop a blank separator line we may have added previously.
            if matches!(out.last(), Some(l) if l.trim().is_empty()) {
                out.pop();
            }
            continue;
        }
        if t == BLOCK_END {
            in_block = false;
            continue;
        }
        if !in_block {
            out.push(line);
        }
    }
    let joined = out.join("\n");
    joined.trim_end_matches('\n').to_string()
}

/// Extract the complete rules block (BEGIN..END markers inclusive) from a
/// script, so a writer that regenerates the script from scratch (the OOF
/// manager) can append it back rather than silently deleting the user's
/// inbox rules. Returns `None` when no block is present.
pub fn rules_segment(script: &str) -> Option<String> {
    let start = script.lines().position(|l| l.trim_end() == BLOCK_BEGIN)?;
    let end = script
        .lines()
        .enumerate()
        .skip(start)
        .find(|(_, l)| l.trim_end() == BLOCK_END)
        .map(|(i, _)| i)?;
    let lines: Vec<&str> = script.lines().collect();
    Some(lines[start..=end].join("\n"))
}

/// Remove only this rule's executable `if` body (used to excise a single
/// rule on ROW_REMOVE while the block is re-rendered wholesale anyway).
/// Exposed for tests; production rendering always regenerates everything.
#[cfg(test)]
pub fn strip_generated(script: &str) -> String {
    strip_rules_block(script)
}

/// Render the full script: existing non-rule content + the freshly
/// generated rules section. `resolve_folder` maps a MAPI folder id (the
/// 64-bit hash derived from the backend mailbox id) back to the JMAP
/// mailbox id used by Sieve `fileinto`. The returned rule list carries the
/// re-derived `ST_ERROR` bits so the caller can persist them.
pub fn render_script(
    existing_script: &str,
    rules: &[StoredRule],
    resolve_folder: &dyn Fn(u64) -> Option<String>,
) -> (String, Vec<StoredRule>) {
    let base = strip_rules_block(existing_script);
    // Sort by sequence ascending (§2.2.1.3.1.2: evaluation order).
    let mut ordered: Vec<StoredRule> = rules.to_vec();
    ordered.sort_by_key(|r| r.sequence);

    let mut body: Vec<String> = Vec::new();
    let mut rendered_rules: Vec<StoredRule> = Vec::with_capacity(ordered.len());
    let mut requires: Vec<&str> = Vec::new();
    for rule in &ordered {
        let mut r = rule.clone();
        match sieve_for_rule(&r, resolve_folder) {
            Ok((mut lines, mut reqs)) => {
                r.state &= !ST_ERROR;
                body.append(&mut lines);
                requires.append(&mut reqs);
            }
            Err(run_time_only) => {
                // Unservable rule: keep it in the table with ST_ERROR set
                // (MS-OXORULE §2.2.1.3.1.3) and emit a comment so the Sieve
                // semantics stay exactly "no action taken".
                r.state = run_time_only;
                body.push(format!("# [rule {} not server-executable]", r.id));
            }
        }
        rendered_rules.push(r);
    }

    let mut out = base;
    if !rendered_rules.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(BLOCK_BEGIN);
        out.push('\n');
        for line in rules_to_block(&rendered_rules) {
            out.push_str(&line);
            out.push('\n');
        }
        out.push_str(BLOCK_GENERATED);
        out.push('\n');
        if !requires.is_empty() {
            requires.sort_unstable();
            requires.dedup();
            let quoted: Vec<String> = requires.iter().map(|c| format!("\"{c}\"")).collect();
            out.push_str(&format!("require [{}];\n", quoted.join(",")));
        }
        for line in &body {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(BLOCK_END);
    }
    (out, rendered_rules)
}

// ---- Sieve translation -------------------------------------------------------

/// Escape a string for embedding in a Sieve double-quoted literal.
fn sieve_quote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Translate a MAPI `SRestriction` to a Sieve test expression (`allof` /
/// `anyof` / `not` / header tests). Returns `None` when any node is not
/// expressible; the caller then marks the rule ST_ERROR rather than
/// running a partial (wrong) condition.
fn sieve_test(r: &SRestriction) -> Option<String> {
    use crate::mapi::restrict::{FuzzyLevel, RelOp};
    match r {
        SRestriction::And(children) => {
            let tests: Option<Vec<String>> = children.iter().map(sieve_test).collect();
            let tests = tests?;
            match tests.len() {
                0 => Some("true".to_string()),
                1 => tests.into_iter().next(),
                _ => Some(format!("allof({})", tests.join(", "))),
            }
        }
        SRestriction::Or(children) => {
            let tests: Option<Vec<String>> = children.iter().map(sieve_test).collect();
            let tests = tests?;
            match tests.len() {
                0 => Some("true".to_string()),
                1 => tests.into_iter().next(),
                _ => Some(format!("anyof({})", tests.join(", "))),
            }
        }
        SRestriction::Not(inner) => Some(format!("not ({})", sieve_test(inner)?)),
        SRestriction::Content {
            fuzzy_level,
            content_tag,
            value,
            ..
        } => {
            let header = match content_tag.property_id {
                0x0037 => "Subject",                              // PR_SUBJECT
                0x0C1A | 0x0C1F | 0x5D01 => "From",               // sender name/smtp
                0x0E04 => "To",                                   // PR_DISPLAY_TO
                0x0E03 => "Cc",                                   // PR_DISPLAY_CC
                0x0076 | 0x0C1E => "To", // intended/the RCPT representative
                0x1000 => return body_test(fuzzy_level.0, value), // PR_BODY
                _ => return None,
            };
            let needle = match value {
                PropertyValue::String(s) | PropertyValue::String8(s) => s.clone(),
                _ => return None,
            };
            let fl = fuzzy_level.0;
            let expr = if fl & FuzzyLevel::FL_PREFIX != 0 {
                format!("header :matches \"{header}\" \"{}*\"", sieve_quote(&needle))
            } else if fl & FuzzyLevel::FL_SUBSTRING != 0 || fl & 0x0000_0003 == 0x0000_0003 {
                format!("header :contains \"{header}\" \"{}\"", sieve_quote(&needle))
            } else if fl == FuzzyLevel::FL_FULLSTRING {
                format!("header :is \"{header}\" \"{}\"", sieve_quote(&needle))
            } else {
                // Unknown fuzzy bits: substring is the safest superset match.
                format!("header :contains \"{header}\" \"{}\"", sieve_quote(&needle))
            };
            Some(expr)
        }
        SRestriction::Property { relop, tag, value } => {
            // Only equality against the string-ish tags is expressible.
            let header = match tag.property_id {
                0x0037 => "Subject",
                0x0C1A | 0x0C1F | 0x5D01 => "From",
                0x0E04 => "To",
                0x0E03 => "Cc",
                0x001A => return None, // PR_MESSAGE_CLASS: no header equivalent
                _ => return None,
            };
            if relop.0 != RelOp::EQ.0 {
                return None;
            }
            let s = match value {
                PropertyValue::String(s) | PropertyValue::String8(s) => s.clone(),
                _ => return None,
            };
            Some(format!("header :is \"{header}\" \"{}\"", sieve_quote(&s)))
        }
        SRestriction::Exist { tag, .. } => {
            // "Exists" on subject/from: header presence test.
            let header = match tag.property_id {
                0x0037 => "Subject",
                0x0C1A | 0x0C1F | 0x5D01 => "From",
                _ => return None,
            };
            Some(format!("exists \"{header}\""))
        }
        // Sub-message restrictions, property-to-property compares, bitmask /
        // size predicates, comment and count restrictions carry no Sieve
        // equivalent; the rule becomes ST_ERROR instead of half-firing.
        _ => None,
    }
}

fn body_test(fl: u16, value: &PropertyValue) -> Option<String> {
    let needle = match value {
        PropertyValue::String(s) | PropertyValue::String8(s) => s.clone(),
        _ => return None,
    };
    let _ = fl; // body has no Sieve :is/:matches variants worth distinguishing
    Some(format!("body :contains \"{}\"", sieve_quote(&needle)))
}

/// One decoded ActionBlock (MS-OXORULE §2.2.5.1), minus the fields we
/// already consumed structurally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleAction {
    Move {
        dest_folder_id: u64,
    },
    Copy {
        dest_folder_id: u64,
    },
    Delete,
    MarkAsRead,
    Forward {
        addresses: Vec<String>,
    },
    /// An action the gateway stores and round-trips but cannot execute
    /// (reply templates, OOF reply, bounce, delegate, tag, defer). The raw
    /// bytes stay in the persisted blob; only the Sieve translation skips
    /// them.
    Unsupported(u8),
}

/// Decode the `PtypRuleAction` blob: 16-bit COUNT + COUNT ActionBlocks.
/// Byte-strict: any truncation is an error so stale rules never half-apply.
pub fn decode_rule_actions(bytes: &[u8]) -> Result<Vec<RuleAction>, DecodeError> {
    let mut cur = Buf::new(bytes);
    let count = usize::from(cur.take_u16_le()?);
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let len = usize::from(cur.take_u16_le()?);
        // ActionLength covers ActionType(1) + Flavor(4) + Flags(4) + data.
        if len < 9 {
            return Err(DecodeError::InvalidValue);
        }
        let op = cur.take_u8()?;
        let _flavor = cur.take_u32_le()?;
        let _flags = cur.take_u32_le()?;
        let data_len = len - 9;
        let data = cur.take_bytes(data_len)?;
        let action = match op {
            0x01 | 0x02 => {
                // OP_MOVE / OP_COPY: FolderInThisStore(1) StoreEIDSize(2)
                // StoreEID(...) FolderEIDSize(2) FolderEID(8).
                let mut dc = Buf::new(data);
                let in_this_store = dc.take_u8()? != 0;
                if !in_this_store {
                    // Cross-mailbox destinations map to no Sieve concept.
                    RuleAction::Unsupported(op)
                } else {
                    let store_eid_size = usize::from(dc.take_u16_le()?);
                    let _store_eid = dc.take_bytes(store_eid_size)?;
                    let folder_eid_size = usize::from(dc.take_u16_le()?);
                    let folder_eid = dc.take_bytes(folder_eid_size)?;
                    if folder_eid.len() != 8 {
                        return Err(DecodeError::InvalidValue);
                    }
                    let mut b = [0u8; 8];
                    b.copy_from_slice(folder_eid);
                    let id = u64::from_le_bytes(b);
                    if op == 0x01 {
                        RuleAction::Move { dest_folder_id: id }
                    } else {
                        RuleAction::Copy { dest_folder_id: id }
                    }
                }
            }
            0x07 => {
                // OP_FORWARD: RecipientCount(4) then RecipientBlockData rows:
                // Reserved(1) NoOfProperties(4) TaggedPropertyValue*.
                let mut dc = Buf::new(data);
                let rcpt_count = dc.take_u32_le()? as usize;
                let mut addrs = Vec::new();
                for _ in 0..rcpt_count {
                    let _reserved = dc.take_u8()?;
                    let prop_count = dc.take_u32_le()? as usize;
                    for _ in 0..prop_count {
                        let tpv = TaggedPropertyValue::decode(&mut dc)?;
                        // PR_EMAIL_ADDRESS (0x3003) / PR_SMTP_ADDRESS (0x39FE).
                        if matches!(tpv.tag.property_id, 0x3003 | 0x39FE)
                            && let PropertyValue::String(s) | PropertyValue::String8(s) = &tpv.value
                        {
                            addrs.push(s.clone());
                        }
                    }
                }
                if addrs.is_empty() {
                    RuleAction::Unsupported(op)
                } else {
                    RuleAction::Forward { addresses: addrs }
                }
            }
            0x0A => RuleAction::Delete,
            0x0B => RuleAction::MarkAsRead,
            // REPLY / OOF_REPLY (template id + blob), DEFER (opaque),
            // BOUNCE (code), DELEGATE (recipient block), TAG (prop blob).
            other => RuleAction::Unsupported(other),
        };
        out.push(action);
    }
    Ok(out)
}

/// Generate the executable Sieve lines for one rule. Err carries the state
/// the rule must be persisted with (ST_ERROR, or the unmodified state when
/// the rule is merely disabled).
fn sieve_for_rule(
    rule: &StoredRule,
    resolve_folder: &dyn Fn(u64) -> Option<String>,
) -> Result<(Vec<String>, Vec<&'static str>), u32> {
    let disabled = rule.state & (ST_ENABLED | ST_ONLY_WHEN_OOF) == 0;
    if disabled {
        // A disabled rule never runs; it is not an error, just inert.
        return Err(rule.state & !ST_ERROR);
    }
    let mut cur = Buf::new(&rule.condition);
    let condition = if rule.condition.is_empty() {
        // No condition: the spec treats an absent restriction as "always".
        Some("true".to_string())
    } else {
        let rst = SRestriction::decode(&mut cur).map_err(|_| rule.state | ST_ERROR)?;
        Some(sieve_test(&rst).ok_or(rule.state | ST_ERROR)?)
    };
    let Some(cond) = condition else {
        return Err(rule.state | ST_ERROR);
    };
    let actions = decode_rule_actions(&rule.actions).map_err(|_| rule.state | ST_ERROR)?;
    let mut cmds: Vec<String> = Vec::new();
    let mut reqs: Vec<&'static str> = Vec::new();
    for a in &actions {
        match a {
            RuleAction::Move { dest_folder_id } => {
                let Some(mailbox) = resolve_folder(*dest_folder_id) else {
                    return Err(rule.state | ST_ERROR);
                };
                cmds.push(format!("fileinto \"{}\";", sieve_quote(&mailbox)));
                reqs.push("fileinto");
            }
            RuleAction::Copy { dest_folder_id } => {
                let Some(mailbox) = resolve_folder(*dest_folder_id) else {
                    return Err(rule.state | ST_ERROR);
                };
                cmds.push(format!("fileinto :copy \"{}\";", sieve_quote(&mailbox)));
                reqs.push("fileinto");
                reqs.push("copy");
            }
            RuleAction::Delete => cmds.push("discard;".to_string()),
            RuleAction::MarkAsRead => {
                cmds.push("addflag \"\\\\Seen\";".to_string());
                reqs.push("imap4flags");
            }
            RuleAction::Forward { addresses } => {
                let list: Vec<String> = addresses
                    .iter()
                    .map(|a| format!("\"{}\"", sieve_quote(a)))
                    .collect();
                cmds.push(format!("redirect {};", list.join(", ")));
            }
            RuleAction::Unsupported(_) => return Err(rule.state | ST_ERROR),
        }
    }
    if cmds.is_empty() {
        // A rule reduced to nothing executable (e.g. only client-side DEFER
        // actions, which New Outlook performs locally) is valid but inert.
        return Err(rule.state & !ST_ERROR);
    }
    if rule.state & ST_EXIT_LEVEL != 0 {
        cmds.push("stop;".to_string());
    }
    let mut lines = vec![format!("# rule {}: {}", rule.id, rule.name)];
    lines.push(format!("if {cond} {{"));
    lines.extend(cmds.iter().map(|c| format!("  {c}")));
    lines.push("}".to_string());
    Ok((lines, reqs))
}

// ---- permission membership mapping (MS-OXCPERM §2.2.7 <-> JMAP sharing) -----

pub const RIGHTS_READ_ANY: u32 = 0x0000_0001;
pub const RIGHTS_CREATE: u32 = 0x0000_0002;
pub const RIGHTS_EDIT_OWNED: u32 = 0x0000_0008;
pub const RIGHTS_DELETE_OWNED: u32 = 0x0000_0010;
pub const RIGHTS_EDIT_ANY: u32 = 0x0000_0020;
pub const RIGHTS_DELETE_ANY: u32 = 0x0000_0040;
pub const RIGHTS_CREATE_SUBFOLDER: u32 = 0x0000_0080;
pub const RIGHTS_FOLDER_OWNER: u32 = 0x0000_0100;
pub const RIGHTS_FOLDER_CONTACT: u32 = 0x0000_0200;
pub const RIGHTS_FOLDER_VISIBLE: u32 = 0x0000_0400;

/// MAPI PidTagMemberRights → JMAP `shareWith` rights object (JMAP sharing
/// model for mailboxes). Rights with no JMAP counterpart (FolderOwner,
/// FolderContact, FreeBusy*) are folded to their nearest expressible
/// equivalent rather than dropped silently.
pub fn mapi_rights_to_jmap(rights: u32) -> serde_json::Value {
    let owner = rights & RIGHTS_FOLDER_OWNER != 0;
    serde_json::json!({
        "mayReadItems": rights & RIGHTS_READ_ANY != 0 || owner,
        "mayAddItems": rights & RIGHTS_CREATE != 0 || owner,
        "mayRemoveItems": rights & (RIGHTS_DELETE_OWNED | RIGHTS_DELETE_ANY) != 0 || owner,
        "maySetSeen": rights & (RIGHTS_EDIT_OWNED | RIGHTS_EDIT_ANY) != 0 || owner,
        "maySetKeywords": rights & (RIGHTS_EDIT_OWNED | RIGHTS_EDIT_ANY) != 0 || owner,
        "mayCreateChild": rights & RIGHTS_CREATE_SUBFOLDER != 0 || owner,
        "mayRename": rights & RIGHTS_EDIT_ANY != 0 || owner,
        "mayDelete": owner,
        "maySubmit": rights & RIGHTS_CREATE != 0 || owner,
    })
}

/// Inverse of [`mapi_rights_to_jmap`] for the table read path.
pub fn jmap_rights_to_mapi(v: &serde_json::Value) -> u32 {
    let b = |k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
    let mut r = RIGHTS_FOLDER_VISIBLE;
    if b("mayReadItems") {
        r |= RIGHTS_READ_ANY;
    }
    if b("mayAddItems") {
        r |= RIGHTS_CREATE;
    }
    if b("mayRemoveItems") {
        r |= RIGHTS_DELETE_OWNED | RIGHTS_DELETE_ANY;
    }
    if b("maySetSeen") || b("maySetKeywords") {
        r |= RIGHTS_EDIT_OWNED | RIGHTS_EDIT_ANY;
    }
    if b("mayCreateChild") {
        r |= RIGHTS_CREATE_SUBFOLDER;
    }
    if b("mayDelete") && b("mayRename") && b("mayReadItems") {
        r |= RIGHTS_FOLDER_OWNER;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_rule() -> StoredRule {
        StoredRule {
            id: 0x1122_3344_5566_7788,
            name: "move alice".to_string(),
            sequence: 1,
            state: ST_ENABLED | ST_EXIT_LEVEL,
            level: 0,
            user_flags: 7,
            provider: "MSFT: Exchange Rules".to_string(),
            provider_data: vec![1, 2, 3],
            // resContent: subject contains "foo"
            condition: {
                let rst = crate::mapi::restrict::SRestriction::Content {
                    fuzzy_level: crate::mapi::restrict::FuzzyLevel::from_u16(
                        crate::mapi::restrict::FuzzyLevel::FL_SUBSTRING,
                    ),
                    content_tag: PropertyTag::new(PropertyType::PTYP_STRING, 0x0037),
                    property_tag: PropertyTag::new(PropertyType::PTYP_STRING, 0x0037),
                    value: PropertyValue::String("foo".into()),
                };
                let mut c = Vec::new();
                rst.encode(&mut c);
                c
            },
            // 1 action: OP_MOVE to folder id 0x0102030405060708, in this store.
            actions: {
                let mut a = Vec::new();
                a.extend_from_slice(&1u16.to_le_bytes());
                let mut block = Vec::new();
                block.push(0x01); // OP_MOVE
                block.extend_from_slice(&0u32.to_le_bytes()); // flavor
                block.extend_from_slice(&0u32.to_le_bytes()); // flags
                block.push(1); // in this store
                block.extend_from_slice(&0u16.to_le_bytes()); // store eid len
                block.extend_from_slice(&8u16.to_le_bytes()); // folder eid len
                block.extend_from_slice(&0x0102_0304_0506_0708u64.to_le_bytes());
                let len = u16::try_from(block.len()).unwrap();
                a.extend_from_slice(&len.to_le_bytes());
                a.extend_from_slice(&block);
                a
            },
        }
    }

    #[test]
    fn block_round_trip() {
        let rules = vec![sample_rule()];
        let block = rules_to_block(&rules).join("\n");
        let script = format!("vacation \"hi\";\n{block}\n# MAPI-RULES-END");
        let parsed = parse_rules(&script);
        assert_eq!(parsed, rules);
    }

    #[test]
    fn strip_preserves_other_content() {
        let script = "require [\"vacation\"];\nvacation \"away\";\n# MAPI-RULES-BEGIN v1\n# ff\n# MAPI-RULES-END\n";
        let stripped = strip_rules_block(script);
        assert!(stripped.contains("vacation \"away\";"));
        assert!(!stripped.contains("MAPI-RULES"));
    }

    #[test]
    fn render_generates_fileinto_and_stop() {
        let (script, rules) = render_script("vacation \"away\";\n", &[sample_rule()], &|id| {
            (id == 0x0102_0304_0506_0708).then(|| "mbox-42".to_string())
        });
        assert!(script.contains("vacation \"away\";"));
        assert!(script.contains("header :contains \"Subject\" \"foo\""));
        assert!(script.contains("fileinto \"mbox-42\";"));
        assert!(script.contains("stop;"));
        assert_eq!(rules[0].state & ST_ERROR, 0);
        // The rendered script re-parses to the same rules.
        let reparsed = parse_rules(&script);
        assert_eq!(reparsed.len(), 1);
        assert_eq!(reparsed[0].name, "move alice");
    }

    #[test]
    fn unresolvable_destination_marks_st_error() {
        let (script, rules) = render_script("", &[sample_rule()], &|_| None);
        assert!(rules[0].state & ST_ERROR != 0);
        assert!(script.contains("not server-executable"));
        assert!(!script.contains("fileinto"));
    }

    #[test]
    fn actions_decode_move() {
        let rule = sample_rule();
        let actions = decode_rule_actions(&rule.actions).expect("decode");
        assert_eq!(
            actions,
            vec![RuleAction::Move {
                dest_folder_id: 0x0102_0304_0506_0708
            }]
        );
    }

    #[test]
    fn rights_mapping_roundtrip_subset() {
        let jmap = mapi_rights_to_jmap(RIGHTS_READ_ANY | RIGHTS_CREATE | RIGHTS_FOLDER_OWNER);
        assert_eq!(jmap["mayReadItems"], serde_json::json!(true));
        assert_eq!(jmap["mayDelete"], serde_json::json!(true));
        let back = jmap_rights_to_mapi(&jmap);
        assert!(back & RIGHTS_READ_ANY != 0);
        assert!(back & RIGHTS_CREATE != 0);
        assert!(back & RIGHTS_FOLDER_OWNER != 0);
        assert!(back & RIGHTS_FOLDER_VISIBLE != 0);
    }
}
