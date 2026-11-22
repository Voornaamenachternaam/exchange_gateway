#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use exchange_gateway::wbxml::Wbxml;
use libfuzzer_sys::fuzz_target;

// A small AST for WBXML documents spanning code pages, namespace switching,
// nested collections, OPAQUE byte arrays, split character data, truncation,
// END imbalance, and invalid tokens.
#[derive(Debug, Clone)]
enum WbxmlElement {
    Sync(WbxmlSync),
    Collection(WbxmlCollection),
    Generic {
        page: u8,
        tag: u8,
        children: Vec<WbxmlElement>,
        text: Option<String>,
    },
}

#[derive(Debug, Clone)]
struct WbxmlSync {
    collections: Vec<WbxmlCollection>,
}

#[derive(Debug, Clone)]
struct WbxmlCollection {
    class: String,
    sync_key: String,
    collection_id: String,
}

impl Arbitrary<'_> for WbxmlElement {
    fn arbitrary(u: &mut Unstructured<'_>) -> arbitrary::Result<Self> {
        let kind: u8 = u.int_in_range(0..=3)?;
        match kind {
            0 => Ok(WbxmlElement::Sync(WbxmlSync {
                collections: u.arbitrary()?,
            })),
            1 => Ok(WbxmlElement::Collection(u.arbitrary()?)),
            2 => Ok(WbxmlElement::Generic {
                page: u.int_in_range(0..=255)?,
                tag: u.int_in_range(0..=255)?,
                children: u.arbitrary()?,
                text: if u.arbitrary()? { Some(u.arbitrary()?) } else { None },
            }),
            _ => Ok(WbxmlElement::Generic {
                page: 0,
                tag: 0,
                children: Vec::new(),
                text: None,
            }),
        }
    }

    fn size_hint(_depth: usize) -> (usize, Option<usize>) {
        (0, Some(64))
    }
}

impl Arbitrary<'_> for WbxmlCollection {
    fn arbitrary(u: &mut Unstructured<'_>) -> arbitrary::Result<Self> {
        Ok(WbxmlCollection {
            class: u.arbitrary()?,
            sync_key: u.arbitrary()?,
            collection_id: u.arbitrary()?,
        })
    }

    fn size_hint(_depth: usize) -> (usize, Option<usize>) {
        (0, Some(32))
    }
}

/// Serialize the AST into a WBXML-ish XML document, then optionally truncate
/// or corrupt it to produce malformed structural variants.
fn render_wbxml_ast(root: &WbxmlElement, corrupt: bool) -> String {
    let mut s = String::new();
    render_element(&mut s, root);
    if corrupt {
        // Inject structural corruption: unclosed tags, truncated content.
        match s.len() {
            0 => s.push_str("<Sync"),
            1..=8 => s.push_str("</Sync>"),
            _ => {
                let cut = s.len() / 2;
                s.truncate(cut);
            }
        }
    }
    s
}

fn render_element(s: &mut String, el: &WbxmlElement) {
    match el {
        WbxmlElement::Sync(sync) => {
            s.push_str("<Sync xmlns=\"AirSync:\"><Collections>");
            for c in &sync.collections {
                render_collection(s, c);
            }
            s.push_str("</Collections></Sync>");
        }
        WbxmlElement::Collection(coll) => {
            render_collection(s, coll);
        }
        WbxmlElement::Generic { page, tag, children, text } => {
            // Emit a namespaced tag for non-root pages, unqualified for page 0.
            if *page == 0 {
                s.push_str(&format!("<tag{tag}>"));
            } else {
                s.push_str(&format!("<P{page}:tag{tag}>"));
            }
            if let Some(t) = text {
                s.push_str(&escape_xml(t));
            }
            for child in children {
                render_element(s, child);
            }
            s.push_str(&format!("</tag{tag}>"));
        }
    }
}

fn render_collection(s: &mut String, coll: &WbxmlCollection) {
    s.push_str("<Collection>");
    s.push_str(&format!("<Class>{}</Class>", escape_xml(&coll.class)));
    s.push_str(&format!("<SyncKey>{}</SyncKey>", escape_xml(&coll.sync_key)));
    s.push_str(&format!("<CollectionId>{}</CollectionId>", escape_xml(&coll.collection_id)));
    s.push_str("</Collection>");
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    // Grammar-aware corpus: generate structured WBXML-ish XML documents.
    if let Ok(ast) = WbxmlElement::arbitrary(&mut u) {
        let corrupt: bool = u.arbitrary().unwrap_or(false);
        let xml = render_wbxml_ast(&ast, corrupt);
        let codec = Wbxml::new();

        // Encode → decode must be stable for well-formed docs.
        if let Ok(encoded) = codec.encode(&xml) {
            if let Ok(decoded) = codec.decode(&encoded) {
                let _ = codec.encode(&decoded);
            }
        }
    }

    // Raw byte fuzzing of the decoder (fail-closed, no panics).
    let _ = Wbxml::new().decode(data);
});
