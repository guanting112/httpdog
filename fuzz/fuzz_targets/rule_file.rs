//! Loading a rule file. Whatever the file contains, loading either succeeds or returns an error;
//! it must never panic. A rule file that does load must then process an event without panicking.
#![no_main]

use httpdog::{config::Config, engine::Engine};
use libfuzzer_sys::fuzz_target;
use serde_json::json;
use std::time::Instant;

fuzz_target!(|data: &[u8]| {
    // The first byte picks YAML or JSON, the way the file extension does
    let Some((&selector, text)) = data.split_first() else {
        return;
    };
    let Ok(text) = std::str::from_utf8(text) else {
        return;
    };
    let Ok(config) = Config::parse(text, selector & 1 == 1) else {
        return;
    };
    let Ok(mut engine) = Engine::from_config(config) else {
        return;
    };

    let event = json!({
        "remote_addr": "203.0.113.7",
        "request_method": "GET",
        "uri": "/.env",
        "request_uri": "/.env?x=1",
        "status": 404,
        "body_bytes_sent": 153,
        "http_referer": "",
        "http_user_agent": "curl/8.5.0",
    });
    let now = Instant::now();
    for alert in engine.process(&event, r#"{"uri":"/.env"}"#, now) {
        assert!(
            !alert.text.chars().any(char::is_control),
            "alert text is not a single line: {:?}",
            alert.text
        );
    }
    engine.report_dropped(1, now);
    engine.sweep(now);
});
