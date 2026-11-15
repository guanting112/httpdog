//! Notifications go out on their own thread, so a slow or dead webhook never blocks the main loop that reads logs.

use ring::hmac;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// A resolved notification target (names are replaced with the actual URL when the config is loaded)
pub enum Target {
    Stderr,
    Webhook {
        url: String,
        /// Signing secret, verified non-empty at load time. A secret like url: it must never appear in logs
        secret: String,
    },
}

pub struct Alert {
    /// The one-line text for people. stderr prints exactly this; a webhook carries it in the `text` field
    pub text: String,
    /// Name of the httpdog instance that raised this alert (`instance` in the rule file). Like targets,
    /// the alert carries it, so a reload that changes the name does not need to tell this thread
    pub instance: Arc<str>,
    /// The parts of the same alert, taken apart; the webhook body is built from these
    pub detail: Detail,
    pub targets: Arc<[Target]>,
}

pub struct Detail {
    pub kind: Kind,
    /// Rule id; always "overload" for overload alerts
    pub rule: String,
    /// The message itself, without the `[httpdog] <id> |` prefix or the trailing count and group
    pub message: String,
    /// When the alert fired (when httpdog received the log, not the time written in the log)
    pub at: SystemTime,
    /// (group_by field name, this group's value)
    pub group: Option<(String, String)>,
    /// (hits accumulated in the window, window length in seconds)
    pub threshold: Option<(usize, u64)>,
    /// Overload alerts: the number of dropped lines reported this time
    pub dropped: Option<u64>,
    /// The log line that triggered the alert
    pub event: Option<Value>,
}

#[derive(Clone, Copy)]
pub enum Kind {
    /// A rule matched
    Rule,
    /// Logs were dropped because the input queue was full
    Overload,
}

/// Version of the webhook body. Bump it only for incompatible field changes (rename, removal, type change);
/// adding a field does not need a bump
const WEBHOOK_VERSION: u32 = 1;

/// The queue is bounded: when the receiver is down for long, drop alerts rather than let memory grow without bound
const QUEUE_SIZE: usize = 1024;
const MAX_ATTEMPTS: u32 = 3;

/// Signature headers, sent with every webhook request
const TIMESTAMP_HEADER: &str = "x-httpdog-timestamp";
const SIGNATURE_HEADER: &str = "x-httpdog-signature";

pub fn spawn() -> (SyncSender<Alert>, JoinHandle<()>) {
    let (tx, rx) = mpsc::sync_channel(QUEUE_SIZE);
    let handle = thread::Builder::new()
        .name("notifier".into())
        .spawn(move || run(rx))
        .expect("cannot spawn the notifier thread");
    (tx, handle)
}

fn run(rx: Receiver<Alert>) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .build()
        .into();

    // The loop ends only after every sender is dropped, so alerts still queued at shutdown are sent first
    for alert in rx {
        for target in alert.targets.iter() {
            match target {
                Target::Stderr => eprintln!("ALERT {}", alert.text),
                Target::Webhook { url, secret } => {
                    // Serialize to bytes ourselves: the signature covers the exact bytes sent,
                    // so the HTTP library must not serialize the body a second time
                    let body = webhook_body(&alert).to_string().into_bytes();
                    send_webhook(&agent, url, &body, secret, &alert.text)
                }
            }
        }
    }
}

/// The body a webhook sends. There is only this one shape: the receiver is a program, so each part has its own field.
/// Every key is always present, with null where it does not apply, so receivers never handle a missing key.
/// This is a public interface: renaming or removing a field breaks other people's receivers,
/// so bump WEBHOOK_VERSION when you do
fn webhook_body(alert: &Alert) -> Value {
    let d = &alert.detail;
    let timestamp =
        d.at.duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
    json!({
        "source": "httpdog",
        "instance": &*alert.instance,
        "version": WEBHOOK_VERSION,
        "kind": match d.kind {
            Kind::Rule => "rule",
            Kind::Overload => "overload",
        },
        "rule": d.rule,
        "timestamp": timestamp,
        "text": alert.text,
        "message": d.message,
        "group_by": d.group.as_ref().map(|(field, _)| field),
        "group": d.group.as_ref().map(|(_, value)| value),
        "hits": d.threshold.map(|(hits, _)| hits),
        "window_secs": d.threshold.map(|(_, window)| window),
        "dropped": d.dropped,
        "event": d.event,
    })
}

/// Signature: `HMAC-SHA256(secret, "<timestamp>.<request body>")`, lowercase hex.
///
/// The timestamp is signed too, so the receiver can reject requests that are too old (captured and replayed);
/// signing the body alone would leave a request valid forever.
/// This is a public interface: receivers verify with this algorithm, and changing it breaks everyone's verification
fn sign(secret: &str, timestamp: u64, body: &[u8]) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    let mut ctx = hmac::Context::with_key(&key);
    ctx.update(timestamp.to_string().as_bytes());
    ctx.update(b".");
    ctx.update(body);
    ctx.sign()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn send_webhook(agent: &ureq::Agent, url: &str, body: &[u8], secret: &str, text: &str) {
    for attempt in 1..=MAX_ATTEMPTS {
        // Take the time again on every attempt: a retry may come seconds later, and a stale timestamp
        // would eat into the receiver's tolerance for nothing
        let timestamp = unix_now();
        let request = agent
            .post(url)
            .header("content-type", "application/json; charset=utf-8")
            .header(TIMESTAMP_HEADER, timestamp.to_string())
            .header(
                SIGNATURE_HEADER,
                format!("sha256={}", sign(secret, timestamp, body)),
            );
        match request.send(body) {
            Ok(_) => return,
            // Note: never print the url or the secret in an error message; both are secrets
            Err(e) if attempt < MAX_ATTEMPTS => {
                eprintln!("webhook attempt {attempt} failed: {e}, retrying shortly");
                thread::sleep(Duration::from_secs(1 << (attempt - 1)));
            }
            Err(e) => {
                eprintln!(
                    "webhook gave up (all {MAX_ATTEMPTS} attempts failed): {e} | unsent alert: {text}"
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alert(detail: Detail) -> Alert {
        Alert {
            text: "one line".into(),
            instance: "web-01".into(),
            detail,
            targets: Arc::from(Vec::new()),
        }
    }

    #[test]
    fn signature_matches_an_independent_implementation() {
        // The expected value was computed with Python:
        //   hmac.new(b"s3cr3t", b'1794416274.{"text":"hi"}', hashlib.sha256).hexdigest()
        let body = br#"{"text":"hi"}"#;
        assert_eq!(
            sign("s3cr3t", 1_794_416_274, body),
            "1728aa567d7ef419cab2bf10fd5e1c764cdb608bd22d6f4d051b2043db19b8fe"
        );
        // Change any of the timestamp, the body or the secret and the signature changes
        assert!(sign("s3cr3t", 1_794_416_275, body).starts_with("e5faade226e917e9"));
        assert_ne!(
            sign("s3cr3t", 1_794_416_274, br#"{"text":"hi!"}"#),
            sign("s3cr3t", 1_794_416_274, body)
        );
        assert_ne!(
            sign("other", 1_794_416_274, body),
            sign("s3cr3t", 1_794_416_274, body)
        );
    }

    #[test]
    fn webhook_body_for_a_rule_alert() {
        let a = alert(Detail {
            kind: Kind::Rule,
            rule: "login-bruteforce".into(),
            message: "198.51.100.9 repeated login failures".into(),
            at: UNIX_EPOCH + Duration::from_secs(1_794_452_645),
            group: Some(("remote_addr".into(), "198.51.100.9".into())),
            threshold: Some((20, 60)),
            dropped: None,
            event: Some(json!({"remote_addr": "198.51.100.9", "status": 401})),
        });
        assert_eq!(
            webhook_body(&a),
            json!({
                "source": "httpdog",
                "instance": "web-01",
                "version": 1,
                "kind": "rule",
                "rule": "login-bruteforce",
                "timestamp": 1_794_452_645u64,
                "text": "one line",
                "message": "198.51.100.9 repeated login failures",
                "group_by": "remote_addr",
                "group": "198.51.100.9",
                "hits": 20,
                "window_secs": 60,
                "dropped": null,
                "event": {"remote_addr": "198.51.100.9", "status": 401},
            })
        );
    }

    #[test]
    fn webhook_body_keeps_every_key_even_when_not_applicable() {
        // A rule with no group_by and no threshold
        let plain = webhook_body(&alert(Detail {
            kind: Kind::Rule,
            rule: "r".into(),
            message: "m".into(),
            at: UNIX_EPOCH,
            group: None,
            threshold: None,
            dropped: None,
            event: Some(json!({})),
        }));
        // overload: there is no log line behind it
        let overload = webhook_body(&alert(Detail {
            kind: Kind::Overload,
            rule: "overload".into(),
            message: "falling behind, dropped 7 log lines".into(),
            at: UNIX_EPOCH,
            group: None,
            threshold: None,
            dropped: Some(7),
            event: None,
        }));
        assert_eq!(overload["kind"], "overload");
        assert_eq!(overload["dropped"], 7);
        assert!(overload["event"].is_null());

        let keys = |v: &Value| -> Vec<String> { v.as_object().unwrap().keys().cloned().collect() };
        assert_eq!(
            keys(&plain),
            keys(&overload),
            "both kinds of alert must have exactly the same keys"
        );
        for key in ["group_by", "group", "hits", "window_secs", "dropped"] {
            assert!(plain[key].is_null(), "{key} should be null");
        }
    }
}
