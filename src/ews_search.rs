// src/ews_search.rs
//
// Translation of EWS search constructs into JMAP Email/query filters.
//
// EWS mailbox search is expressed through two independent mechanisms
// (MS-OXWSSRCH, MS-OXWSMSG, MS-OXOSRCH):
//
//   1. A `QueryString` using Microsoft's Advanced Query Syntax (AQS) — a
//      free-form, user-typed search language such as `subject:report
//      from:alice hasattachment:true`.
//
//   2. A structured `Restriction` whose `SearchExpression` subtree encodes
//      boolean logic (`And`/`Or`/`Not`) over property comparisons
//      (`Contains`, `IsEqualTo`, `IsGreaterThan`, ...) keyed by a
//      `FieldURI`/`IndexedFieldURI`/`ExtendedFieldURI`.
//
// Both are translated into the JMAP Email/query `filter` value defined by
// RFC 8620 §5.5 and RFC 8621 §4.4.1: a `FilterOperator` (the `operator`
// is one of `AND`/`OR`/`NOT`, with a freely nestable `conditions` array of
// `FilterOperator`/`FilterCondition` entries) or a single `FilterCondition`
// (a property->value object such as `{"subject": "..."}` or
// `{"inMailbox": "<id>"}`).
//
// The gateway combines the resulting filter with its mandatory `inMailbox`
// restriction by wrapping both under a top-level `AND` operator
// (see [`and_with_in_mailbox`]).
//
// Addressability of unsupported/mapped fields falls back to the JMAP
// `text` full-text operator so that a search never silently degrades to
// "return everything" (the pre-existing behaviour this module replaces).

use serde_json::{Value, json};

/// Map an AQS `keyword:` prefix (lower-cased, trailing `:` stripped) to a
/// JMAP `FilterCondition` property name. Returns `None` when the keyword
/// has no direct JMAP email field equivalent and should instead be handled
/// by [`aqs_keyword_condition`].
fn aqs_field(name: &str) -> Option<&'static str> {
    match name {
        "subject" | "subjecttitle" => Some("subject"),
        "from" | "sender" => Some("from"),
        "to" | "recipients" => Some("to"),
        "cc" => Some("cc"),
        "bcc" => Some("bcc"),
        "body" | "contents" | "content" => Some("body"),
        "text" | "all" => Some("text"),
        _ => None,
    }
}

/// Translate a single AQS `keyword:value` token into a JMAP `FilterCondition`
/// (or `FilterOperator` for the negated/read-state/keyword cases).
///
/// Returns `None` when the keyword is not understood, in which case the
/// caller should fall back to a `text` search so that the user's input is
/// still honoured.
fn aqs_keyword_condition(keyword: &str, value: &str) -> Option<Value> {
    let kw = keyword.trim().to_lowercase();
    let value = value.trim();

    // Strip surrounding quotes / brackets that AQS permits around values.
    let value = strip_aqs_wrappers(value);

    if let Some(field) = aqs_field(&kw) {
        if value.is_empty() {
            return None;
        }
        return Some(json!({ field: value }));
    }

    match kw.as_str() {
        "hasattachment" | "hasattachments" | "has" => Some(json!({
            "hasAttachment": aqs_truthy(&value)
        })),
        "attachment" | "attachments" => Some(json!({ "hasAttachment": true })),
        "isread" | "read" | "seen" => read_state_condition(&value),
        "isunread" | "unread" => read_state_condition(&format!("not:{value}")),
        "category" | "categories" => {
            if value.is_empty() {
                None
            } else {
                Some(json!({ "hasKeyword": value }))
            }
        }
        "hasflag" | "flagged" | "isflagged" => Some(json!({ "hasKeyword": "$flagged" })),
        _ => None,
    }
}

/// Strip AQS value wrappers: surrounding single/double quotes or a
/// surrounding `( ... )` group that AQS sometimes emits around a value.
fn strip_aqs_wrappers(value: &str) -> String {
    let mut v = value.trim();
    if v.len() >= 2 {
        let first = v.as_bytes()[0];
        let last = v.as_bytes()[v.len() - 1];
        let matched_pair = (first == b'"' && last == b'"')
            || (first == b'\'' && last == b'\'')
            || (first == b'(' && last == b')');
        if matched_pair {
            v = &v[1..v.len() - 1];
        }
    }
    v.trim().to_string()
}

/// Interpret an AQS boolean literal (`true`/`yes`/`1`/`false`/`no`/`0`).
fn aqs_truthy(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "false" | "no" | "0" | "off" | ""
    )
}

/// Build a read/unread FilterCondition.
///
/// The read state maps to the JMAP `$seen` keyword (RFC 8621 §2.1). An
/// "is read" search yields `hasKeyword: "$seen"`; "is unread" yields
/// `notKeyword: "$seen"`. A leading `not:` prefix (used internally for the
/// `isunread`/`unread` keywords) or a falsy value inverts the sense.
fn read_state_condition(value: &str) -> Option<Value> {
    let v = value.trim();
    let negated = v.starts_with("not:") || !aqs_truthy(v);
    if negated {
        Some(json!({ "notKeyword": "$seen" }))
    } else {
        Some(json!({ "hasKeyword": "$seen" }))
    }
}

/// Token produced by the AQS lexer.
#[derive(Debug, Clone, PartialEq)]
enum Token {
    /// A bare search term (plain word or quoted phrase).
    Term(String),
    /// A `keyword:value` pair (keyword includes the trailing colon).
    Field(String, String),
    And,
    Or,
    Not,
    /// An explicit parenthesised grouping opener/closer.
    LParen,
    RParen,
}

/// Lex an AQS query string into [`Token`]s.
///
/// Handles quoted phrases, `keyword:` prefixes, the boolean operators
/// `AND`/`OR`/`NOT` (case-insensitive), `-`/`!` as `NOT`, and parentheses
/// for grouping.
fn aqs_lex(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let b = input.as_bytes();
    let mut i = 0usize;
    let n = b.len();

    while i < n {
        let c = b[i];
        match c {
            b' ' | b'\t' | b'\r' | b'\n' => {
                i += 1;
            }
            b'(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            b')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            b'-' | b'!' => {
                tokens.push(Token::Not);
                i += 1;
            }
            b'"' | b'\'' => {
                // Quoted phrase.
                let quote = c;
                i += 1;
                let start = i;
                while i < n && b[i] != quote {
                    i += 1;
                }
                let phrase = &input[start..i];
                tokens.push(Token::Term(phrase.to_string()));
                if i < n {
                    i += 1; // closing quote
                }
            }
            _ => {
                let start = i;
                while i < n && !b" \t\r\n()\"'".contains(&b[i]) {
                    i += 1;
                }
                let word = &input[start..i];
                // Check for `keyword:value`.
                if let Some(colon) = word.find(':') {
                    let kw = &word[..colon];
                    let val = &word[colon + 1..];
                    if val.is_empty() || val.starts_with(':') || val.starts_with('>') || val.starts_with('<') {
                        // A leading comparison operator or empty value =>
                        // treat the whole word as a plain term.
                        tokens.push(Token::Term(word.to_string()));
                    } else {
                        tokens.push(Token::Field(kw.to_string(), val.to_string()));
                    }
                } else {
                    match word.to_ascii_uppercase().as_str() {
                        "AND" => tokens.push(Token::And),
                        "OR" => tokens.push(Token::Or),
                        "NOT" => tokens.push(Token::Not),
                        _ => tokens.push(Token::Term(word.to_string())),
                    }
                }
            }
        }
    }

    tokens
}

/// Recursive-descent parser state for AQS.
struct AqsParser {
    tokens: Vec<Token>,
    pos: usize,
}

impl AqsParser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    /// Parse a full AQS expression. Implicit `AND` binds tighter than `OR`
    /// with the standard precedence: `orExpr := andExpr (OR andExpr)*`;
    /// `andExpr := unary (AND? unary)*`; a sequence of adjacent terms is an
    /// implicit AND.
    fn parse_or(&mut self) -> Option<Value> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Some(Token::Or)) {
            self.next();
            let right = self.parse_and()?;
            left = mk_or(left, right);
        }
        Some(left)
    }

    fn parse_and(&mut self) -> Option<Value> {
        let mut left = self.parse_unary()?;
        loop {
            match self.peek() {
                Some(Token::And) => {
                    self.next();
                    let right = self.parse_unary()?;
                    left = mk_and(left, right);
                }
                Some(Token::Term(_)) | Some(Token::Field(..)) | Some(Token::Not)
                | Some(Token::LParen) => {
                    // Implicit AND between adjacent terms.
                    let right = self.parse_unary()?;
                    left = mk_and(left, right);
                }
                _ => break,
            }
        }
        Some(left)
    }

    fn parse_unary(&mut self) -> Option<Value> {
        match self.peek().cloned() {
            Some(Token::Not) => {
                self.next();
                let child = self.parse_unary()?;
                Some(mk_not(child))
            }
            Some(Token::LParen) => {
                self.next();
                let inner = self.parse_or()?;
                // Expect a closing paren.
                if matches!(self.peek(), Some(Token::RParen)) {
                    self.next();
                }
                Some(inner)
            }
            Some(Token::Term(value)) => {
                self.next();
                Some(term_condition(&value))
            }
            Some(Token::Field(kw, value)) => {
                self.next();
                // Fall back to a text search when the keyword is unknown so
                // the input is still honoured rather than silently dropped.
                Some(aqs_keyword_condition(&kw, &value).unwrap_or_else(|| term_condition(&value)))
            }
            _ => None,
        }
    }
}

/// Build a JMAP FilterCondition from a bare AQS term (full-text search).
fn term_condition(value: &str) -> Value {
    let v = strip_aqs_wrappers(value);
    if v.is_empty() {
        json!({})
    } else {
        json!({ "text": v })
    }
}

/// Combine two filters under a JMAP `AND` `FilterOperator` (flattening an
/// existing top-level `AND` whenever possible).
fn mk_and(a: Value, b: Value) -> Value {
    and_conditions(vec![a, b])
}

/// Combine two filters under a JMAP `OR` `FilterOperator`.
fn mk_or(a: Value, b: Value) -> Value {
    json!({ "operator": "OR", "conditions": [a, b] })
}

/// Wrap a filter in a JMAP `NOT` `FilterOperator`.
fn mk_not(a: Value) -> Value {
    json!({ "operator": "NOT", "conditions": [a] })
}

/// Build a JMAP `FilterOperator` with `operator: "AND"` from a list of
/// filter values, flattening any top-level `AND` operators already present.
fn and_conditions(filters: Vec<Value>) -> Value {
    let mut conditions = Vec::new();
    for f in filters {
        if let Some(op) = f.get("operator").and_then(|o| o.as_str())
            && op == "AND"
            && let Some(cs) = f.get("conditions").and_then(|c| c.as_array())
        {
            conditions.extend(cs.iter().cloned());
        } else {
            conditions.push(f);
        }
    }
    json!({ "operator": "AND", "conditions": conditions })
}

/// Translate an EWS AQS `QueryString` into a JMAP Email/query filter value,
/// or `None` when the query is empty/whitespace.
pub fn ews_aqs_to_jmap_filter(query: &str) -> Option<Value> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    let tokens = aqs_lex(q);
    if tokens.is_empty() {
        return None;
    }
    let mut parser = AqsParser::new(tokens);
    parser.parse_or()
}

// ---------------------------------------------------------------------------
// EWS Restriction (structured SearchExpression) -> JMAP filter
// ---------------------------------------------------------------------------

/// Determine whether an element has the given EWS local name.
///
/// roxmltree exposes the *local* element name via `tag_name().name()` (the
/// XML namespace prefix is reported separately), so a plain string compare is
/// correct regardless of the `m:`/`t:` prefix the caller used.
fn is_t(elem: &roxmltree::Node, name: &str) -> bool {
    elem.tag_name().name() == name
}

/// Map an EWS `FieldURI` (PathToUnindexedField) value to a JMAP email
/// `FilterCondition` property name.
fn field_uri_to_jmap(field_uri: &str) -> Option<&'static str> {
    match field_uri {
        "item:Subject" => Some("subject"),
        "item:Body" | "item:TextBody" => Some("body"),
        "item:AllProperties" => Some("text"),
        "message:From" => Some("from"),
        "message:ToRecipients" | "item:DisplayTo" => Some("to"),
        "message:CcRecipients" | "item:DisplayCc" => Some("cc"),
        "message:BccRecipients" => Some("bcc"),
        "message:ConversationTopic" | "conversation:ConversationTopic" => Some("subject"),
        _ => None,
    }
}

/// Extract the `FieldURI` attribute from a `FieldURI`/`IndexedFieldURI`/
/// `ExtendedFieldURI` element. For `ExtendedFieldURI` the effective field is
/// carried by `PropertyName` (a distinguished set id) or, failing that,
/// `PropertyTag`; we prefer named property sets and fall back to the
/// `FieldURI`/`IndexedFieldURI` attribute for the simpler path types.
fn extract_field_uri(node: roxmltree::Node) -> Option<String> {
    // Extended property URIs carry a `PropertyName` (distinguished set) that
    // maps to well-known named properties such as Subject/Body.
    if let Some(pn) = node.attribute("PropertyName") {
        return Some(pn.to_string());
    }
    if let Some(pt) = node.attribute("PropertyTag") {
        return Some(pt.to_string());
    }
    node.attribute("FieldURI").map(|v| v.to_string())
}

/// Extract the `Value` attribute of the first `Constant` child.
fn constant_value(node: roxmltree::Node) -> Option<String> {
    for c in node.descendants().filter(|c| c.is_element() && is_t(c, "Constant")) {
        if let Some(v) = c.attribute("Value") {
            return Some(v.to_string());
        }
    }
    None
}

/// Find the first child element carrying a field URI (a `FieldURI`,
/// `IndexedFieldURI`, or `ExtendedFieldURI` element).
fn field_uri_child<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
) -> Option<roxmltree::Node<'a, 'input>> {
    node.children().find(|c| {
        c.is_element()
            && (is_t(c, "FieldURI") || is_t(c, "IndexedFieldURI") || is_t(c, "ExtendedFieldURI"))
    })
}

/// Translate a list of boolean-expression children into a JMAP operator's
/// `conditions` array (skipping any nested abstract `SearchExpression`
/// wrapper elements and unrecognised nodes).
fn translate_children(node: roxmltree::Node) -> Vec<Value> {
    node.children()
        .filter(|c| c.is_element() && !is_t(c, "SearchExpression"))
        .filter_map(translate_search_expression)
        .collect()
}

/// Translate a single EWS `SearchExpression` node into a JMAP filter.
fn translate_search_expression(node: roxmltree::Node) -> Option<Value> {
    if !node.is_element() {
        return None;
    }
    let name = node.tag_name().name();

    match name {
        "And" => collapse_operator("AND", translate_children(node)),
        "Or" => collapse_operator("OR", translate_children(node)),
        "Not" => {
            let child = node
                .children()
                .find(|c| c.is_element() && !is_t(c, "SearchExpression"))
                .and_then(translate_search_expression)?;
            Some(mk_not(child))
        }
        "Contains" | "IsEqualTo" => {
            let field_uri = field_uri_child(node).and_then(extract_field_uri)?;
            let value = constant_value(node)?;
            let field = field_uri_to_jmap(&field_uri).unwrap_or("text");
            Some(json!({ field: value }))
        }
        // Exists / Excludes / range comparisons have no direct JMAP email
        // equivalent and are intentionally omitted rather than emitting a
        // filter that silently returns everything.
        _ => None,
    }
}

/// Collapse a translated `AND`/`OR` condition list into a JMAP `FilterOperator`
/// (or a bare condition when only one child survives translation).
fn collapse_operator(operator: &str, conditions: Vec<Value>) -> Option<Value> {
    match conditions.len() {
        0 => None,
        1 => conditions.into_iter().next(),
        _ => Some(json!({ "operator": operator, "conditions": conditions })),
    }
}

/// Translate an EWS `Restriction` element (its `SearchExpression` child)
/// into a JMAP filter value, or `None` when no recognised expression is
/// present.
pub fn ews_restriction_to_jmap_filter(xml: &str) -> Option<Value> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    for node in doc.descendants() {
        if node.is_element() && is_t(&node, "Restriction") {
            let expr = node
                .children()
                .find(|c| c.is_element() && !is_t(c, "SearchExpression"))
                .or_else(|| node.children().find(|c| c.is_element()));
            if let Some(expr) = expr {
                return translate_search_expression(expr);
            }
        }
    }
    None
}

/// Combine a mandatory `inMailbox` condition with an optional search filter
/// under a single `AND` `FilterOperator`.
///
/// Returns just the search filter (or `None`) when the mailbox id is empty.
pub fn combine_with_in_mailbox(mailbox_id: &str, search: Option<Value>) -> Option<Value> {
    search.map(|search| {
        and_conditions(vec![json!({ "inMailbox": mailbox_id }), search])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_aqs_single_bare_term() {
        let f = ews_aqs_to_jmap_filter("report").unwrap();
        assert_eq!(f, json!({ "text": "report" }));
    }

    #[test]
    fn test_aqs_subject_keyword() {
        let f = ews_aqs_to_jmap_filter("subject:report").unwrap();
        assert_eq!(f, json!({ "subject": "report" }));
    }

    #[test]
    fn test_aqs_from_keyword() {
        let f = ews_aqs_to_jmap_filter("from:alice@example.com").unwrap();
        assert_eq!(f, json!({ "from": "alice@example.com" }));
    }

    #[test]
    fn test_aqs_implicit_and() {
        let f = ews_aqs_to_jmap_filter("subject:report from:alice").unwrap();
        assert_eq!(
            f,
            json!({ "operator": "AND", "conditions": [
                { "subject": "report" },
                { "from": "alice" }
            ]})
        );
    }

    #[test]
    fn test_aqs_explicit_or() {
        let f = ews_aqs_to_jmap_filter("from:alice OR from:bob").unwrap();
        assert_eq!(
            f,
            json!({ "operator": "OR", "conditions": [
                { "from": "alice" },
                { "from": "bob" }
            ]})
        );
    }

    #[test]
    fn test_aqs_has_attachment_true() {
        let f = ews_aqs_to_jmap_filter("hasattachment:true").unwrap();
        assert_eq!(f, json!({ "hasAttachment": true }));
    }

    #[test]
    fn test_aqs_has_attachment_false() {
        let f = ews_aqs_to_jmap_filter("hasattachment:false").unwrap();
        assert_eq!(f, json!({ "hasAttachment": false }));
    }

    #[test]
    fn test_aqs_quoted_phrase() {
        let f = ews_aqs_to_jmap_filter("\"quarterly report\"").unwrap();
        assert_eq!(f, json!({ "text": "quarterly report" }));
    }

    #[test]
    fn test_aqs_negation() {
        let f = ews_aqs_to_jmap_filter("NOT subject:meeting").unwrap();
        assert_eq!(
            f,
            json!({ "operator": "NOT", "conditions": [{ "subject": "meeting" }] })
        );
    }

    #[test]
    fn test_aqs_grouping() {
        let f = ews_aqs_to_jmap_filter("(from:alice OR from:bob) subject:report").unwrap();
        assert_eq!(
            f,
            json!({ "operator": "AND", "conditions": [
                { "operator": "OR", "conditions": [
                    { "from": "alice" },
                    { "from": "bob" }
                ]},
                { "subject": "report" }
            ]})
        );
    }

    #[test]
    fn test_aqs_empty_returns_none() {
        assert!(ews_aqs_to_jmap_filter("   ").is_none());
        assert!(ews_aqs_to_jmap_filter("").is_none());
    }

    #[test]
    fn test_aqs_unknown_keyword_falls_back_to_text() {
        let f = ews_aqs_to_jmap_filter("importance:high").unwrap();
        // "importance" has no JMAP equivalent, so it degrades to a text search.
        assert_eq!(f, json!({ "text": "high" }));
    }

    const TNS: &str = "http://schemas.microsoft.com/exchange/services/2006/types";

    #[test]
    fn test_restriction_contains_subject() {
        let xml = r#"<t:Restriction xmlns:t="TNS"><t:Contains><t:FieldURI FieldURI="item:Subject"/><t:Constant Value="report"/></t:Contains></t:Restriction>"#
            .replace("TNS", TNS);
        assert_eq!(
            ews_restriction_to_jmap_filter(&xml).unwrap(),
            json!({ "subject": "report" })
        );
    }

    #[test]
    fn test_restriction_contains_from() {
        let xml = r#"<t:Restriction xmlns:t="TNS"><t:Contains><t:FieldURI FieldURI="message:From"/><t:Constant Value="alice"/></t:Contains></t:Restriction>"#
            .replace("TNS", TNS);
        assert_eq!(
            ews_restriction_to_jmap_filter(&xml).unwrap(),
            json!({ "from": "alice" })
        );
    }

    #[test]
    fn test_restriction_and() {
        let xml = r#"<t:Restriction xmlns:t="TNS"><t:And>
            <t:Contains><t:FieldURI FieldURI="item:Subject"/><t:Constant Value="report"/></t:Contains>
            <t:Contains><t:FieldURI FieldURI="message:From"/><t:Constant Value="alice"/></t:Contains>
        </t:And></t:Restriction>"#
            .replace("TNS", TNS);
        assert_eq!(
            ews_restriction_to_jmap_filter(&xml).unwrap(),
            json!({ "operator": "AND", "conditions": [
                { "subject": "report" },
                { "from": "alice" }
            ]})
        );
    }

    #[test]
    fn test_restriction_or() {
        let xml = r#"<t:Restriction xmlns:t="TNS"><t:Or>
            <t:Contains><t:FieldURI FieldURI="message:From"/><t:Constant Value="alice"/></t:Contains>
            <t:Contains><t:FieldURI FieldURI="message:From"/><t:Constant Value="bob"/></t:Contains>
        </t:Or></t:Restriction>"#
            .replace("TNS", TNS);
        assert_eq!(
            ews_restriction_to_jmap_filter(&xml).unwrap(),
            json!({ "operator": "OR", "conditions": [
                { "from": "alice" },
                { "from": "bob" }
            ]})
        );
    }

    #[test]
    fn test_restriction_not() {
        let xml = r#"<t:Restriction xmlns:t="TNS"><t:Not><t:Contains><t:FieldURI FieldURI="item:Subject"/><t:Constant Value="meeting"/></t:Contains></t:Not></t:Restriction>"#
            .replace("TNS", TNS);
        assert_eq!(
            ews_restriction_to_jmap_filter(&xml).unwrap(),
            json!({ "operator": "NOT", "conditions": [{ "subject": "meeting" }] })
        );
    }

    #[test]
    fn test_restriction_invalid_returns_none() {
        assert!(ews_restriction_to_jmap_filter("not xml <").is_none());
    }

    #[test]
    fn test_combine_in_mailbox_and_search() {
        let search = ews_aqs_to_jmap_filter("subject:report").unwrap();
        let combined = combine_with_in_mailbox("mbx-1", Some(search)).unwrap();
        assert_eq!(
            combined,
            json!({ "operator": "AND", "conditions": [
                { "inMailbox": "mbx-1" },
                { "subject": "report" }
            ]})
        );
    }

    #[test]
    fn test_combine_in_mailbox_only_returns_none() {
        assert!(combine_with_in_mailbox("mbx-1", None).is_none());
    }
}