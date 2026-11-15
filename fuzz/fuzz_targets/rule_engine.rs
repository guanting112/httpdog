//! The whole path a log line takes: parse, match against rules that use every operator, count
//! thresholds, apply cooldowns and render the message. Nothing in it may panic, and the text of
//! an alert must stay on one line whatever the request put in the log.
#![no_main]

use httpdog::{
    config::{Config, Format},
    engine::Engine,
    format,
};
use libfuzzer_sys::fuzz_target;
use std::time::{Duration, Instant};

// Rules for both formats use only fields that combined has, so one rule set serves both.
const RULES: &str = r#"
notifiers: { out: { type: stderr } }
overload: { notify: [out], cooldown: 2s }
rules:
  - id: strings
    when:
      any:
        - { field: uri, op: eq, value: /login }
        - { field: uri, op: ne, value: / }
        - { field: uri, op: prefix, value: /.git/ }
        - { field: uri, op: suffix, value: .env }
        - { field: request_uri, op: contains, value: "../" }
        - { field: request_uri, op: regex, value: '(?i)union.+select|\.(sql|bak|tar\.gz)$' }
        - { field: request_method, op: in, value: [POST, PUT, DELETE] }
    group_by: remote_addr
    cooldown: 0s
    notify: [out]
    message: "{remote_addr} {request_method} {uri} {request_uri} {status} {nope} {"
  - id: numbers
    when:
      all:
        - { field: status, op: gte, value: 400 }
        - { field: status, op: lt, value: 600 }
        - not: { field: body_bytes_sent, op: gt, value: 1000000 }
        - any:
            - { field: status, op: in, value: [401, 403, "404"] }
            - { field: body_bytes_sent, op: lte, value: 0 }
    group_by: uri
    threshold: { count: 2, window: 3s }
    cooldown: 1s
    notify: [out]
  - id: presence
    when:
      all:
        - { field: http_user_agent, op: exists }
        - not: { field: http_referer, op: eq, value: "" }
    threshold: { count: 3, window: 2s }
    notify: [out]
    message: "{http_user_agent} via {http_referer}"
"#;

fn engine(format: &str) -> Engine {
    let text = format!("format: {format}\n{RULES}");
    let config = Config::parse(&text, false).expect("the built-in rules are valid");
    Engine::from_config(config).expect("the built-in rules are valid")
}

fuzz_target!(|data: &[u8]| {
    // The first byte picks the format; the rest is the log, one line per event
    let Some((&selector, log)) = data.split_first() else {
        return;
    };
    let (format, mut engine) = if selector & 1 == 0 {
        (Format::Json, engine("json"))
    } else {
        (Format::Combined, engine("combined"))
    };

    let start = Instant::now();
    let log = String::from_utf8_lossy(log);
    for (i, line) in log.split('\n').enumerate() {
        // Each line arrives a second after the previous one, so windows and cooldowns both expire
        let now = start + Duration::from_secs(i as u64);
        if let Some((event, raw)) = format::parse(format, line) {
            for alert in engine.process(&event, raw, now) {
                assert!(alert.text.starts_with("[httpdog] "), "{:?}", alert.text);
                assert!(
                    !alert.text.chars().any(char::is_control),
                    "alert text is not a single line: {:?}",
                    alert.text
                );
            }
        }
        engine.report_dropped(i as u64 % 3, now);
        engine.sweep(now);
    }
});
