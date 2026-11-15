//! The json log parser, fed arbitrary bytes the way a UDP packet reaches it.
//! Parsing may fail (None) but must never panic, and what it returns must be what the engine expects.
#![no_main]

use httpdog::{config::Format, format};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The input threads do the same lossy conversion before handing a line to the parser
    let line = String::from_utf8_lossy(data);
    if let Some((event, raw)) = format::parse(Format::Json, &line) {
        assert!(event.is_object(), "an event is always a JSON object");
        assert!(line.contains(raw), "the raw content is a part of the line");
        // request_path is derived from request_uri whenever that is a string
        if event["request_uri"].is_string() {
            assert!(event.get("request_path").is_some(), "request_path is missing");
        }
    }
});
