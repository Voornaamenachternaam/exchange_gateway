#![no_main]

use arbitrary::Arbitrary;
use exchange_gateway::wbxml::Wbxml;
use exchange_gateway::xml_escape;
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct Document {
    declare_namespace: bool,
    collections: Vec<Collection>,
}

#[derive(Arbitrary, Debug)]
struct Collection {
    key: Text,
    id: Text,
}

#[derive(Arbitrary, Debug)]
enum Text {
    Empty,
    Ascii,
    XmlSpecial,
    Unicode,
    Digits,
}

impl Text {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Empty => "",
            Self::Ascii => "plain ASCII",
            Self::XmlSpecial => "<>&\"'",
            Self::Unicode => "élan 東京 🙂",
            Self::Digits => "0123456789abcdef",
        }
    }
}

fuzz_target!(|document: Document| {
    let collections = document
        .collections
        .iter()
        .map(|collection| {
            format!(
                "<Collection><Class>Email</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId></Collection>",
                xml_escape(collection.key.as_str()),
                xml_escape(collection.id.as_str()),
            )
        })
        .collect::<String>();
    let namespace = if document.declare_namespace {
        " xmlns=\"AirSync:\""
    } else {
        ""
    };
    let xml = format!("<Sync{namespace}><Collections>{collections}</Collections></Sync>");
    let codec = Wbxml::new();
    let first = codec.encode(&xml).expect("generated document must encode");
    let decoded = codec.decode(&first).expect("generated WBXML must decode");
    let second = codec
        .encode(&decoded)
        .expect("decoded generated WBXML must re-encode");
    assert_eq!(first, second, "WBXML wire bytes changed after a round-trip");
});
