//! The raw shape of the config file: only deserializes YAML / JSON into data structures.
//! Validation and compilation (precompiled regexes, type checks) happen in engine.rs.

use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::{collections::HashMap, fs, path::Path, time::Duration};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// UDP listen address; nginx's syslog:server= must point here
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Log format, json by default
    #[serde(default)]
    pub format: Format,
    /// Name of this httpdog instance. It is sent with every webhook so the receiver can tell which one fired;
    /// "unknown" when omitted
    pub instance: Option<String>,
    #[serde(default)]
    pub notifiers: HashMap<String, NotifierDef>,
    /// Who to notify when logs are dropped because the input queue is full; when omitted, drops are only logged
    pub overload: Option<OverloadDef>,
    pub rules: Vec<RuleDef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverloadDef {
    pub notify: Vec<String>,
    /// Minimum interval between two alerts; drops in between accumulate into the next one. Default 1m
    #[serde(default, deserialize_with = "de_opt_duration")]
    pub cooldown: Option<Duration>,
}

fn default_listen() -> String {
    "127.0.0.1:5514".into()
}

/// Format of each log line. Parsing lives in format.rs
#[derive(Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// One JSON object per line; the fields come from nginx's log_format
    #[default]
    Json,
    /// The default combined format of nginx / Apache; the fields are fixed
    Combined,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum NotifierDef {
    /// Print to stderr (ends up in journald); handy while testing rules.
    /// Written as `Stderr {}` rather than `Stderr`: serde does not apply deny_unknown_fields to a variant
    /// without fields, so a stray key (say, a url written here) would be silently ignored
    Stderr {},
    /// POST the alert as JSON to a URL, for your own receiver. See webhook_body in notify.rs for the fields
    Webhook {
        /// The URL written inline (for testing only)
        url: Option<String>,
        /// Read the URL from an environment variable. Use this in production; secrets stay out of the config file
        url_env: Option<String>,
        /// Signing secret written inline (for testing only)
        secret: Option<String>,
        /// Read the signing secret from an environment variable. One of secret / secret_env is required;
        /// every request carries X-Httpdog-Signature
        secret_env: Option<String>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleDef {
    pub id: String,
    pub when: CondDef,
    /// Field that counters and cooldowns are grouped by, e.g. remote_addr = each IP is counted separately
    pub group_by: Option<String>,
    /// Omitted = a single match fires
    pub threshold: Option<ThresholdDef>,
    /// Fire at most once per group within this period, default 1m
    #[serde(default, deserialize_with = "de_opt_duration")]
    pub cooldown: Option<Duration>,
    pub notify: Vec<String>,
    /// Message template; {field} is replaced with that log line's value. When omitted, the raw log line is attached
    pub message: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThresholdDef {
    pub count: usize,
    #[serde(deserialize_with = "de_duration")]
    pub window: Duration,
}

/// Condition tree: all / any / not nest freely; a leaf is "field op value"
#[derive(Deserialize)]
#[serde(try_from = "RawCond")]
pub enum CondDef {
    All { all: Vec<CondDef> },
    Any { any: Vec<CondDef> },
    Not { not: Box<CondDef> },
    Leaf { field: String, op: Op, value: Value },
}

/// Read into an intermediate struct where every key is optional, then decide which kind of node it is.
/// A few more lines than #[serde(untagged)], but on a typo the error names the key and the line
/// instead of just saying "did not match any variant".
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCond {
    all: Option<Vec<CondDef>>,
    any: Option<Vec<CondDef>>,
    not: Option<Box<CondDef>>,
    field: Option<String>,
    op: Option<Op>,
    #[serde(default)]
    value: Value,
}

impl TryFrom<RawCond> for CondDef {
    type Error = String;

    fn try_from(raw: RawCond) -> Result<Self, String> {
        match raw {
            RawCond {
                all: Some(all),
                any: None,
                not: None,
                field: None,
                op: None,
                value: Value::Null,
            } => Ok(CondDef::All { all }),
            RawCond {
                all: None,
                any: Some(any),
                not: None,
                field: None,
                op: None,
                value: Value::Null,
            } => Ok(CondDef::Any { any }),
            RawCond {
                all: None,
                any: None,
                not: Some(not),
                field: None,
                op: None,
                value: Value::Null,
            } => Ok(CondDef::Not { not }),
            RawCond {
                all: None,
                any: None,
                not: None,
                field: Some(field),
                op: Some(op),
                value,
            } => Ok(CondDef::Leaf { field, op, value }),
            _ => Err("a condition must be one of all / any / not, or { field, op, value }".into()),
        }
    }
}

#[derive(Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Eq,
    Ne,
    In,
    Prefix,
    Suffix,
    Contains,
    Regex,
    Gt,
    Gte,
    Lt,
    Lte,
    Exists,
}

impl Config {
    /// Pick the format by extension: .json is JSON, everything else is YAML
    pub fn load(path: &Path) -> Result<Config, String> {
        let text =
            fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let is_json = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json"));
        Config::parse(&text, is_json)
    }

    /// Parse the text of a rule file. Split out of `load` so the fuzz targets can feed it text directly
    pub fn parse(text: &str, is_json: bool) -> Result<Config, String> {
        if is_json {
            serde_json::from_str(text).map_err(|e| e.to_string())
        } else {
            serde_yaml_ng::from_str(text).map_err(|e| e.to_string())
        }
    }
}

/// "30s" / "10m" / "2h"; a bare number means seconds
fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, unit) = match s.find(|c: char| !c.is_ascii_digit()) {
        Some(i) => s.split_at(i),
        None => (s, "s"),
    };
    let n: u64 = num
        .parse()
        .map_err(|_| format!("invalid duration: {s:?}"))?;
    let secs = match unit.trim() {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        other => return Err(format!("unknown time unit {other:?} (use s / m / h)")),
    };
    Ok(Duration::from_secs(secs))
}

fn de_duration<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    // Accept both "60s" and 60
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Text(String),
        Secs(u64),
    }
    match Raw::deserialize(d)? {
        Raw::Text(s) => parse_duration(&s).map_err(serde::de::Error::custom),
        Raw::Secs(n) => Ok(Duration::from_secs(n)),
    }
}

fn de_opt_duration<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
    de_duration(d).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("10m").unwrap(), Duration::from_secs(600));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("45").unwrap(), Duration::from_secs(45));
        assert!(parse_duration("5 days").is_err());
        assert!(parse_duration("abc").is_err());
    }
}
