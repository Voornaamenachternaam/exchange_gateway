#![no_main]

use exchange_gateway::wbxml::Wbxml;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    let codec = Wbxml::new();
    if let Ok(xml) = codec.decode(bytes)
        && !bytes.first().is_some_and(|byte| *byte == b'<')
        && let Ok(encoded) = codec.encode(&xml)
    {
        let decoded = codec.decode(&encoded).expect("canonical WBXML must decode");
        let encoded_again = codec
            .encode(&decoded)
            .expect("decoded canonical WBXML must re-encode");
        assert_eq!(
            encoded_again, encoded,
            "canonical WBXML re-encoding was not stable"
        );
    }
});
