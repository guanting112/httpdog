//! Input formats: turn one log line into the event (a JSON object) the rule engine works on.
//!
//! - json: the log line is already JSON (nginx writes it with `log_format ... escape=json`)
//! - combined: the default combined format of nginx / Apache, parsed into the same field names as the json variant,
//!   so one rule works for both formats (except for fields combined does not have)
//!
//! A line that fails to parse returns None; the main loop counts it as "unparseable" and carries on.

use crate::config::Format;
use serde_json::{Map, Value};

/// The fields a parsed combined line always has. The format is fixed, so a rule that refers to a field
/// not listed here is rejected at load time; json fields come from the user's log_format and cannot be checked.
pub const COMBINED_FIELDS: &[&str] = &[
    "remote_addr",
    "remote_user",
    "time_local",
    "request",
    "request_method",
    "request_uri",
    "uri",
    "server_protocol",
    "status",
    "body_bytes_sent",
    "http_referer",
    "http_user_agent",
];

/// Returns (event, raw content). The raw content is the line with the syslog header removed;
/// a rule without a message uses the start of it as the alert text.
pub fn parse(format: Format, line: &str) -> Option<(Value, &str)> {
    match format {
        Format::Json => parse_json(line),
        Format::Combined => parse_combined(line),
    }
}

/// What nginx sends over syslog looks like `<190>Nov 11 13:42:01 nginx: {...}`,
/// so parsing starts at the first `{` and the syslog header before it is ignored.
fn parse_json(line: &str) -> Option<(Value, &str)> {
    let raw = line[line.find('{')?..].trim_end();
    let event: Value = serde_json::from_str(raw).ok()?;
    event.is_object().then_some((event, raw))
}

/// One combined line:
/// `203.0.113.7 - alice [11/Nov/2026:13:50:01 +0800] "GET /a?x=1 HTTP/1.1" 200 5120 "-" "curl/8.5.0"`
///
/// The last two quoted fields (Referer, User-Agent) are optional, which also lets Apache's common format through;
/// extra custom fields after them are ignored.
fn parse_combined(line: &str) -> Option<(Value, &str)> {
    let raw = strip_syslog_header(line).trim_end();

    let (remote_addr, rest) = raw.split_once(' ')?;
    // The second field (identd) is almost always "-"; it is not kept
    let (ident_user, rest) = rest.split_once(" [")?;
    let (_ident, remote_user) = ident_user.split_once(' ')?;
    let (time_local, rest) = rest.split_once("] ")?;
    let (request, rest) = quoted(rest)?;
    let (status, rest) = rest.strip_prefix(' ')?.split_once(' ')?;
    let status: u16 = status.parse().ok()?;
    let (bytes, rest) = rest.split_once(' ').unwrap_or((rest, ""));
    // Apache's %b writes "-" when the response has no body
    let bytes: u64 = if bytes == "-" { 0 } else { bytes.parse().ok()? };
    let (referer, rest) = quoted(rest).unwrap_or_default();
    let (user_agent, _extra) = rest.strip_prefix(' ').and_then(quoted).unwrap_or_default();

    let request = dash_to_empty(request);
    let (method, request_uri, protocol) = split_request(&request);
    let uri = request_uri.split('?').next().unwrap_or_default();

    let mut event = Map::new();
    let mut put = |key: &str, value: Value| event.insert(key.to_owned(), value);
    put("remote_addr", remote_addr.into());
    put("remote_user", dash_to_empty(remote_user.to_owned()).into());
    put("time_local", time_local.into());
    put("request_method", method.into());
    put("request_uri", request_uri.into());
    put("uri", uri.into());
    put("server_protocol", protocol.into());
    put("status", status.into());
    put("body_bytes_sent", bytes.into());
    put("http_referer", dash_to_empty(referer).into());
    put("http_user_agent", dash_to_empty(user_agent).into());
    put("request", request.into());
    Some((Value::Object(event), raw))
}

/// Lines nginx sends over syslog start with a header like `<190>Nov 11 13:50:01 nginx: ` (RFC 3164).
/// Only lines starting with `<` are touched; the header ends at the first ": " after the tag.
/// Lines read straight from a file or stdin have no header and are returned as they are.
fn strip_syslog_header(line: &str) -> &str {
    if !line.starts_with('<') {
        return line;
    }
    line.split_once(": ").map_or(line, |(_, msg)| msg)
}

/// Read one double-quoted field. Returns (unescaped content, what is left after the closing quote).
///
/// A `"` inside the quotes is always escaped: nginx writes `\x22`, Apache writes `\"`,
/// so an unescaped `"` ends the field.
/// Unescaping is deliberately limited to printable characters: a `\xHH` that is a control character is left as is,
/// and so are Apache's `\n` and `\t`,
/// so an attacker cannot put a newline in a URL or User-Agent and forge another line in the log through the alert text.
fn quoted(s: &str) -> Option<(String, &str)> {
    let body = s.strip_prefix('"')?;
    let bytes = body.as_bytes();
    let mut out = Vec::with_capacity(bytes.len().min(256));
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                // `"` is ASCII, so i + 1 always falls on a char boundary
                let text = String::from_utf8_lossy(&out).into_owned();
                return Some((text, &body[i + 1..]));
            }
            b'\\' => match *bytes.get(i + 1)? {
                b'x' => match hex_byte(bytes.get(i + 2..i + 4)) {
                    Some(b) if b >= 0x20 && b != 0x7f => {
                        out.push(b);
                        i += 4;
                    }
                    _ => {
                        out.extend_from_slice(b"\\x");
                        i += 2;
                    }
                },
                c @ (b'"' | b'\\') => {
                    out.push(c);
                    i += 2;
                }
                c => {
                    out.extend_from_slice(&[b'\\', c]);
                    i += 2;
                }
            },
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    None // No closing quote: the line was truncated
}

fn hex_byte(digits: Option<&[u8]>) -> Option<u8> {
    let text = std::str::from_utf8(digits?).ok()?;
    u8::from_str_radix(text, 16).ok()
}

/// A field without a value is written as "-" in combined. Turn it into an empty string,
/// the same as the json format (escape=json) does
fn dash_to_empty(s: String) -> String {
    if s == "-" { String::new() } else { s }
}

/// Split `GET /a?x=1 HTTP/1.1` into (method, path with query string, protocol).
/// When it cannot be split (empty request, garbage, a TLS handshake sent to the HTTP port) all three are empty strings;
/// the full content is still in the `request` field.
fn split_request(request: &str) -> (&str, &str, &str) {
    let Some((method, rest)) = request.split_once(' ') else {
        return ("", "", "");
    };
    match rest.rsplit_once(' ') {
        Some((uri, protocol)) if protocol.starts_with("HTTP/") => (method, uri, protocol),
        // An HTTP/0.9 request has no protocol part
        _ => (method, rest, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn combined(line: &str) -> Value {
        parse(Format::Combined, line)
            .unwrap_or_else(|| panic!("should have parsed: {line}"))
            .0
    }

    #[test]
    fn json_with_and_without_syslog_header() {
        let (ev, raw) = parse(
            Format::Json,
            r#"<190>Nov 11 13:42:01 nginx: {"uri":"/a","status":200}"#,
        )
        .unwrap();
        assert_eq!(ev["uri"], "/a");
        assert_eq!(raw, r#"{"uri":"/a","status":200}"#);

        assert!(parse(Format::Json, r#"{"uri":"/b"}"#).is_some());
        assert!(parse(Format::Json, "<190>Nov 11 13:42:01 nginx: plain text line").is_none());
        assert!(parse(Format::Json, r#"<190>Nov 11 nginx: {"truncated":"#).is_none());
        assert!(parse(Format::Json, "").is_none());
    }

    #[test]
    fn combined_line_has_the_same_field_names_as_json() {
        let line = r#"203.0.113.7 - alice [11/Nov/2026:13:50:01 +0800] "GET /products?page=2 HTTP/1.1" 200 5120 "https://example.com/" "Mozilla/5.0 (X11; Linux x86_64)""#;
        let (ev, raw) = parse(Format::Combined, line).unwrap();
        assert_eq!(raw, line);
        assert_eq!(
            ev,
            json!({
                "remote_addr": "203.0.113.7",
                "remote_user": "alice",
                "time_local": "11/Nov/2026:13:50:01 +0800",
                "request": "GET /products?page=2 HTTP/1.1",
                "request_method": "GET",
                "request_uri": "/products?page=2",
                "uri": "/products",
                "server_protocol": "HTTP/1.1",
                "status": 200,
                "body_bytes_sent": 5120,
                "http_referer": "https://example.com/",
                "http_user_agent": "Mozilla/5.0 (X11; Linux x86_64)",
            })
        );
        // Every field must be listed in COMBINED_FIELDS, or the field check at load time would reject valid rules
        let keys: Vec<&str> = ev.as_object().unwrap().keys().map(String::as_str).collect();
        for key in &keys {
            assert!(
                COMBINED_FIELDS.contains(key),
                "{key} is not listed in COMBINED_FIELDS"
            );
        }
        assert_eq!(keys.len(), COMBINED_FIELDS.len());
    }

    #[test]
    fn combined_strips_the_syslog_header() {
        let body = r#"192.0.2.44 - - [11/Nov/2026:13:50:06 +0800] "GET /backup/db.sql HTTP/1.1" 404 153 "-" "python-requests/2.32""#;
        for header in [
            "<190>Nov 11 13:50:06 nginx: ",
            "<190>Nov  1 03:05:06 web01 nginx: ",
        ] {
            let line = format!("{header}{body}\n");
            let (ev, raw) = parse(Format::Combined, &line).unwrap();
            assert_eq!(raw, body);
            assert_eq!(ev["remote_addr"], "192.0.2.44");
            assert_eq!(ev["uri"], "/backup/db.sql");
        }
        // A ": " inside the content must not be cut when the line has no syslog header
        let ev = combined(
            r#"::1 - - [11/Nov/2026:13:50:06 +0800] "GET /a HTTP/1.1" 200 1 "-" "note: hello""#,
        );
        assert_eq!(ev["remote_addr"], "::1");
        assert_eq!(ev["http_user_agent"], "note: hello");
    }

    #[test]
    fn combined_dash_means_empty() {
        let ev = combined(
            r#"203.0.113.7 - - [11/Nov/2026:13:50:01 +0800] "GET / HTTP/1.1" 304 0 "-" "-""#,
        );
        assert_eq!(ev["remote_user"], "");
        assert_eq!(ev["http_referer"], "");
        assert_eq!(ev["http_user_agent"], "");
        assert_eq!(ev["body_bytes_sent"], 0);

        // Apache's %b: "-" when there is no body
        let ev = combined(
            r#"203.0.113.7 - - [11/Nov/2026:13:50:01 +0800] "GET / HTTP/1.1" 304 - "-" "-""#,
        );
        assert_eq!(ev["body_bytes_sent"], 0);
    }

    #[test]
    fn combined_unescapes_printable_characters_only() {
        // nginx: a quote is \x22, a backslash is \x5C, non-ASCII is \xHH byte by byte
        let ev = combined(
            r#"203.0.113.7 - - [11/Nov/2026:13:50:01 +0800] "GET /caf\xC3\xA9?q=\x22a\x5Cb\x22 HTTP/1.1" 200 1 "-" "say \x22hi\x22""#,
        );
        assert_eq!(ev["request_uri"], r#"/café?q="a\b""#);
        assert_eq!(ev["uri"], "/café");
        assert_eq!(ev["http_user_agent"], r#"say "hi""#);

        // Apache: a quote is \", a backslash is \\
        let ev = combined(
            r#"203.0.113.7 - - [11/Nov/2026:13:50:01 +0800] "GET /a HTTP/1.1" 200 1 "-" "say \"hi\" c:\\dir""#,
        );
        assert_eq!(ev["http_user_agent"], r#"say "hi" c:\dir"#);

        // Control characters are not unescaped, so a newline cannot slip into the alert text
        let ev = combined(
            r#"203.0.113.7 - - [11/Nov/2026:13:50:01 +0800] "GET /a\x0AALERT\x00 HTTP/1.1" 200 1 "-" "a\nb\tc\x7F""#,
        );
        assert_eq!(ev["request_uri"], r"/a\x0AALERT\x00");
        assert_eq!(ev["http_user_agent"], r"a\nb\tc\x7F");
        // \x not followed by two hex digits: kept as is
        let ev = combined(
            r#"203.0.113.7 - - [11/Nov/2026:13:50:01 +0800] "GET /a HTTP/1.1" 200 1 "-" "\xZZ \x4""#,
        );
        assert_eq!(ev["http_user_agent"], r"\xZZ \x4");
    }

    #[test]
    fn combined_odd_request_lines() {
        let line = |request: &str| {
            format!(r#"203.0.113.7 - - [11/Nov/2026:13:50:01 +0800] "{request}" 400 157 "-" "-""#)
        };
        // Empty request (connected and sent nothing)
        let ev = combined(&line("-"));
        assert_eq!(ev["request"], "");
        assert_eq!(ev["request_method"], "");
        assert_eq!(ev["uri"], "");
        assert_eq!(ev["status"], 400);

        // A TLS handshake sent to the HTTP port: all garbage, no spaces
        let ev = combined(&line(r"\x16\x03\x01\x02\x00\x01"));
        assert_eq!(ev["request"], r"\x16\x03\x01\x02\x00\x01");
        assert_eq!(ev["request_method"], "");

        // HTTP/0.9
        let ev = combined(&line("GET /old"));
        assert_eq!(ev["request_method"], "GET");
        assert_eq!(ev["request_uri"], "/old");
        assert_eq!(ev["server_protocol"], "");

        // A space inside the path
        let ev = combined(&line("GET /a b?c=d e HTTP/1.1"));
        assert_eq!(ev["request_uri"], "/a b?c=d e");
        assert_eq!(ev["uri"], "/a b");
        assert_eq!(ev["server_protocol"], "HTTP/1.1");
    }

    #[test]
    fn combined_accepts_common_format_and_extra_fields() {
        // Apache's common: no Referer and no User-Agent
        let ev =
            combined(r#"203.0.113.7 - - [11/Nov/2026:13:50:01 +0800] "GET /a HTTP/1.1" 200 5120"#);
        assert_eq!(ev["status"], 200);
        assert_eq!(ev["body_bytes_sent"], 5120);
        assert_eq!(ev["http_referer"], "");
        assert_eq!(ev["http_user_agent"], "");

        // Extra custom fields at the end (commonly X-Forwarded-For or request_time) are ignored
        let ev = combined(
            r#"203.0.113.7 - - [11/Nov/2026:13:50:01 +0800] "GET /a HTTP/1.1" 200 5120 "-" "curl/8.5.0" "198.51.100.1" 0.042"#,
        );
        assert_eq!(ev["http_user_agent"], "curl/8.5.0");
    }

    #[test]
    fn combined_rejects_lines_that_are_not_combined() {
        for line in [
            "",
            "plain text line",
            r#"{"uri":"/a","status":200}"#,
            // The status code is not a number
            r#"203.0.113.7 - - [11/Nov/2026:13:50:01 +0800] "GET /a HTTP/1.1" abc 5120 "-" "-""#,
            // The request's quote is not closed (the UDP packet was truncated)
            r#"203.0.113.7 - - [11/Nov/2026:13:50:01 +0800] "GET /a HTTP/1.1"#,
            // The time is missing
            r#"203.0.113.7 - - "GET /a HTTP/1.1" 200 5120 "-" "-""#,
        ] {
            assert!(
                parse(Format::Combined, line).is_none(),
                "should not have parsed: {line}"
            );
        }
    }
}
