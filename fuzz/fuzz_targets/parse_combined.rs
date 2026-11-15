//! The hand-written combined log parser, fed arbitrary bytes the way a UDP packet reaches it.
//! Parsing may fail (None) but must never panic. A line that parses must have every documented field,
//! and unescaping must not turn an escape sequence into a control character.
#![no_main]

use httpdog::{config::Format, format};
use libfuzzer_sys::fuzz_target;
use serde_json::Value;

fn is_ascii_control(c: char) -> bool {
    (c as u32) < 0x20 || c == '\x7f'
}

fuzz_target!(|data: &[u8]| {
    let line = String::from_utf8_lossy(data);
    let Some((event, raw)) = format::parse(Format::Combined, &line) else {
        return;
    };
    assert!(line.contains(raw), "the raw content is a part of the line");

    let fields = event.as_object().expect("an event is always a JSON object");
    assert_eq!(fields.len(), format::COMBINED_FIELDS.len());
    for name in format::COMBINED_FIELDS {
        assert!(fields.contains_key(*name), "{name} is missing");
    }
    assert!(fields["status"].is_number());
    assert!(fields["body_bytes_sent"].is_number());

    // The promise that keeps a request from forging a line in the log: when the line itself has
    // no control characters, no field has one either, whatever escape sequences it contains
    if !line.chars().any(is_ascii_control) {
        for (name, value) in fields {
            if let Value::String(s) = value {
                assert!(
                    !s.chars().any(is_ascii_control),
                    "{name} gained a control character: {s:?}"
                );
            }
        }
    }
});
