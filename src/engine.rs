//! Rule engine: at load time the config is compiled into internal structures (precompiled regexes, type checks);
//! after that each log line is only matched, the config is never interpreted again.

use crate::config::{CondDef, Config, Format, NotifierDef, Op, OverloadDef, RuleDef};
use crate::format::COMBINED_FIELDS;
use crate::notify::{Alert, Detail, Kind, Target};
use regex::Regex;
use serde_json::Value;
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

const DEFAULT_COOLDOWN: Duration = Duration::from_secs(60);
const RAW_PREVIEW_CHARS: usize = 300;
/// The name overload alerts use in messages and in the structured body; matches the overload key of the rule file
const OVERLOAD_ID: &str = "overload";
/// The name used when the rule file has no instance
const DEFAULT_INSTANCE: &str = "unknown";
const INSTANCE_MAX_CHARS: usize = 64;

pub struct Engine {
    /// The main loop parses each line according to this. It lives in the engine so that a reload,
    /// which swaps the engine, swaps the format with it
    format: Format,
    /// Name of this httpdog instance, carried by every alert
    instance: Arc<str>,
    rules: Vec<Rule>,
    /// (rule index, group value) -> counter and cooldown state
    state: HashMap<(usize, String), KeyState>,
    /// Set only when the config has an overload section
    overload: Option<Overload>,
}

/// Alert settings and state for logs dropped on the input side
struct Overload {
    cooldown: Duration,
    targets: Arc<[Target]>,
    /// Lines already dropped but not yet reported in an alert
    pending: u64,
    last_fired: Option<Instant>,
}

struct Rule {
    id: String,
    cond: Node,
    group_by: Option<String>,
    threshold: Option<(usize, Duration)>,
    cooldown: Duration,
    targets: Arc<[Target]>,
    message: Option<String>,
}

#[derive(Default)]
struct KeyState {
    hits: VecDeque<Instant>,
    last_fired: Option<Instant>,
}

enum Node {
    All(Vec<Node>),
    Any(Vec<Node>),
    Not(Box<Node>),
    Leaf { field: String, matcher: Matcher },
}

enum Matcher {
    Eq(Value),
    Ne(Value),
    In(Vec<Value>),
    Prefix(String),
    Suffix(String),
    Contains(String),
    Regex(Regex),
    Gt(f64),
    Gte(f64),
    Lt(f64),
    Lte(f64),
    Exists,
}

impl Engine {
    /// Compile the config into an engine; every error is reported here, none is left for run time
    pub fn from_config(cfg: Config) -> Result<Engine, String> {
        let instance = resolve_instance(cfg.instance).map_err(|e| format!("instance: {e}"))?;
        let mut notifiers: HashMap<String, Target> = HashMap::new();
        for (name, def) in cfg.notifiers {
            let target = resolve_notifier(def).map_err(|e| format!("notifier {name:?}: {e}"))?;
            notifiers.insert(name, target);
        }
        // Several rules may point at the same notifier, so targets are shared through an Arc
        let notifiers: HashMap<String, Arc<Target>> = notifiers
            .into_iter()
            .map(|(k, v)| (k, Arc::new(v)))
            .collect();

        let mut seen = HashSet::new();
        let mut rules = Vec::with_capacity(cfg.rules.len());
        for def in cfg.rules {
            if !seen.insert(def.id.clone()) {
                return Err(format!("duplicate rule id: {:?}", def.id));
            }
            let id = def.id.clone();
            rules.push(
                compile_rule(def, cfg.format, &notifiers)
                    .map_err(|e| format!("rule {id:?}: {e}"))?,
            );
        }
        let overload = cfg
            .overload
            .map(|def| compile_overload(def, &notifiers).map_err(|e| format!("overload: {e}")))
            .transpose()?;
        Ok(Engine {
            format: cfg.format,
            instance,
            rules,
            state: HashMap::new(),
            overload,
        })
    }

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    pub fn format(&self) -> Format {
        self.format
    }

    /// Process one log line and return the alerts it fires (usually none)
    pub fn process(&mut self, event: &Value, raw: &str, now: Instant) -> Vec<Alert> {
        let mut alerts = Vec::new();
        for (idx, rule) in self.rules.iter().enumerate() {
            if !eval(&rule.cond, event) {
                continue;
            }
            let group = match &rule.group_by {
                Some(field) => event
                    .get(field)
                    .map(|v| as_text(v).into_owned())
                    .unwrap_or_default(),
                None => String::new(),
            };
            let st = self.state.entry((idx, group.clone())).or_default();

            // Threshold: fires only once count hits accumulate within the window
            let mut hits = 1;
            if let Some((count, window)) = rule.threshold {
                st.hits.push_back(now);
                while st
                    .hits
                    .front()
                    .is_some_and(|t| now.duration_since(*t) > window)
                {
                    st.hits.pop_front();
                }
                if st.hits.len() < count {
                    continue;
                }
                hits = st.hits.len();
                st.hits.clear(); // Start counting again after firing
            }

            // Cooldown: fire at most once per group within the cooldown
            if st
                .last_fired
                .is_some_and(|t| now.duration_since(t) < rule.cooldown)
            {
                continue;
            }
            st.last_fired = Some(now);

            let message = match &rule.message {
                Some(tpl) => render(tpl, event),
                None => raw.chars().take(RAW_PREVIEW_CHARS).collect(),
            };
            alerts.push(Alert {
                text: format_alert(rule, &message, &group, hits),
                instance: self.instance.clone(),
                detail: Detail {
                    kind: Kind::Rule,
                    rule: rule.id.clone(),
                    message,
                    at: SystemTime::now(),
                    group: rule.group_by.clone().map(|field| (field, group)),
                    threshold: rule.threshold.map(|(_, window)| (hits, window.as_secs())),
                    dropped: None,
                    event: Some(event.clone()),
                },
                targets: rule.targets.clone(),
            });
        }
        alerts
    }

    /// Sweep expired state regularly; otherwise every IP ever seen keeps an entry in memory forever
    pub fn sweep(&mut self, now: Instant) {
        let rules = &self.rules;
        self.state.retain(|(idx, _), st| {
            let rule = &rules[*idx];
            if let Some((_, window)) = rule.threshold {
                while st
                    .hits
                    .front()
                    .is_some_and(|t| now.duration_since(*t) > window)
                {
                    st.hits.pop_front();
                }
            }
            let cooling = st
                .last_fired
                .is_some_and(|t| now.duration_since(t) < rule.cooldown);
            cooling || !st.hits.is_empty()
        });
    }

    /// Report logs the input side dropped because the queue was full. `dropped` is the number dropped since the last call.
    /// Call it regularly, even with nothing new (pass 0): a count accumulated during the cooldown
    /// can only be reported once the cooldown ends.
    /// Always returns None when overload is not configured.
    pub fn report_dropped(&mut self, dropped: u64, now: Instant) -> Option<Alert> {
        let ov = self.overload.as_mut()?;
        ov.pending = ov.pending.saturating_add(dropped);
        if ov.pending == 0 {
            return None;
        }
        if ov
            .last_fired
            .is_some_and(|t| now.duration_since(t) < ov.cooldown)
        {
            return None;
        }
        ov.last_fired = Some(now);
        let count = std::mem::take(&mut ov.pending);
        let message = format!("falling behind, dropped {count} log lines");
        Some(Alert {
            text: format!("[httpdog] {OVERLOAD_ID} | {message}"),
            instance: self.instance.clone(),
            detail: Detail {
                kind: Kind::Overload,
                rule: OVERLOAD_ID.into(),
                message,
                at: SystemTime::now(),
                group: None,
                threshold: None,
                dropped: Some(count),
                event: None,
            },
            targets: ov.targets.clone(),
        })
    }
}

/// The name appears verbatim in alert text, so empty names, overlong names and control characters
/// that would break the line are rejected
fn resolve_instance(instance: Option<String>) -> Result<Arc<str>, String> {
    let Some(name) = instance else {
        return Ok(DEFAULT_INSTANCE.into());
    };
    if name.trim().is_empty() {
        return Err(
            "must not be empty (remove the line if you do not need it; \"unknown\" is used)".into(),
        );
    }
    if name.chars().count() > INSTANCE_MAX_CHARS {
        return Err(format!("too long, at most {INSTANCE_MAX_CHARS} characters"));
    }
    if name.chars().any(char::is_control) {
        return Err("must not contain newlines or other control characters".into());
    }
    Ok(name.into())
}

fn resolve_notifier(def: NotifierDef) -> Result<Target, String> {
    match def {
        NotifierDef::Stderr {} => Ok(Target::Stderr),
        NotifierDef::Webhook {
            url,
            url_env,
            secret,
            secret_env,
        } => {
            let url = match (url, url_env) {
                (Some(url), None) => url,
                (None, Some(var)) => env_value(&var)?,
                _ => return Err("exactly one of url / url_env is required".into()),
            };
            // Signing is mandatory: a webhook without a secret is never allowed to start.
            // Without a signature anyone who knows the URL can send fake alerts, and that kind of
            // misconfiguration is hard to notice afterwards
            let secret = match (secret, secret_env) {
                (Some(secret), None) => secret,
                (None, Some(var)) => env_value(&var)?,
                (None, None) => {
                    return Err(
                        "a webhook must have a signing secret: add secret_env (or secret, for testing)".into(),
                    );
                }
                (Some(_), Some(_)) => {
                    return Err("only one of secret / secret_env may be set".into());
                }
            };
            if secret.is_empty() {
                return Err("secret must not be an empty string".into());
            }
            Ok(Target::Webhook { url, secret })
        }
    }
}

/// Read an environment variable. Unset and empty are both errors: a missing secret must be caught at load time
fn env_value(var: &str) -> Result<String, String> {
    match std::env::var(var) {
        Ok(value) if !value.is_empty() => Ok(value),
        _ => Err(format!("environment variable {var} is not set")),
    }
}

/// Replace the names in a notify list with the actual notification targets
fn resolve_targets(
    names: &[String],
    notifiers: &HashMap<String, Arc<Target>>,
) -> Result<Arc<[Target]>, String> {
    if names.is_empty() {
        return Err("notify must not be empty".into());
    }
    let mut targets = Vec::with_capacity(names.len());
    for name in names {
        let target = notifiers
            .get(name)
            .ok_or_else(|| format!("notifier {name:?} not found"))?;
        targets.push(match target.as_ref() {
            Target::Stderr => Target::Stderr,
            Target::Webhook { url, secret } => Target::Webhook {
                url: url.clone(),
                secret: secret.clone(),
            },
        });
    }
    Ok(targets.into())
}

fn compile_overload(
    def: OverloadDef,
    notifiers: &HashMap<String, Arc<Target>>,
) -> Result<Overload, String> {
    Ok(Overload {
        cooldown: def.cooldown.unwrap_or(DEFAULT_COOLDOWN),
        targets: resolve_targets(&def.notify, notifiers)?,
        pending: 0,
        last_fired: None,
    })
}

/// The combined fields are fixed, so a mistyped field name can be rejected at load time.
/// json fields come from the user's log_format, so there is nothing to check against
fn check_field(format: Format, field: &str) -> Result<(), String> {
    if format == Format::Combined && !COMBINED_FIELDS.contains(&field) {
        return Err(format!(
            "the combined format has no field {field:?} (available: {})",
            COMBINED_FIELDS.join(", ")
        ));
    }
    Ok(())
}

fn compile_rule(
    def: RuleDef,
    format: Format,
    notifiers: &HashMap<String, Arc<Target>>,
) -> Result<Rule, String> {
    let targets = resolve_targets(&def.notify, notifiers)?;
    if let Some(field) = &def.group_by {
        check_field(format, field).map_err(|e| format!("group_by: {e}"))?;
    }
    let threshold = match def.threshold {
        Some(t) if t.count == 0 || t.window.is_zero() => {
            return Err("threshold count and window must both be greater than 0".into());
        }
        Some(t) => Some((t.count, t.window)),
        None => None,
    };
    Ok(Rule {
        id: def.id,
        cond: compile_cond(def.when, format)?,
        group_by: def.group_by,
        threshold,
        cooldown: def.cooldown.unwrap_or(DEFAULT_COOLDOWN),
        targets,
        message: def.message,
    })
}

fn compile_cond(def: CondDef, format: Format) -> Result<Node, String> {
    Ok(match def {
        CondDef::All { all } => Node::All(
            all.into_iter()
                .map(|c| compile_cond(c, format))
                .collect::<Result<_, _>>()?,
        ),
        CondDef::Any { any } => Node::Any(
            any.into_iter()
                .map(|c| compile_cond(c, format))
                .collect::<Result<_, _>>()?,
        ),
        CondDef::Not { not } => Node::Not(Box::new(compile_cond(*not, format)?)),
        CondDef::Leaf { field, op, value } => {
            check_field(format, &field)?;
            let matcher = compile_matcher(op, value)
                .map_err(|e| format!("{op:?} on field {field:?}: {e}"))?;
            Node::Leaf { field, matcher }
        }
    })
}

fn compile_matcher(op: Op, value: Value) -> Result<Matcher, String> {
    let text = |v: &Value| {
        v.as_str()
            .map(str::to_owned)
            .ok_or("value must be a string".to_string())
    };
    let num = |v: &Value| v.as_f64().ok_or("value must be a number".to_string());
    let scalar = |v: Value| match v {
        Value::String(_) | Value::Number(_) | Value::Bool(_) => Ok(v),
        _ => Err("value must be a string, number or boolean".to_string()),
    };
    Ok(match op {
        Op::Eq => Matcher::Eq(scalar(value)?),
        Op::Ne => Matcher::Ne(scalar(value)?),
        Op::In => match value {
            Value::Array(items) if !items.is_empty() => {
                Matcher::In(items.into_iter().map(scalar).collect::<Result<_, _>>()?)
            }
            _ => return Err("value must be a non-empty list".into()),
        },
        Op::Prefix => Matcher::Prefix(text(&value)?),
        Op::Suffix => Matcher::Suffix(text(&value)?),
        Op::Contains => Matcher::Contains(text(&value)?),
        Op::Regex => Matcher::Regex(Regex::new(&text(&value)?).map_err(|e| e.to_string())?),
        Op::Gt => Matcher::Gt(num(&value)?),
        Op::Gte => Matcher::Gte(num(&value)?),
        Op::Lt => Matcher::Lt(num(&value)?),
        Op::Lte => Matcher::Lte(num(&value)?),
        Op::Exists => Matcher::Exists,
    })
}

fn eval(node: &Node, event: &Value) -> bool {
    match node {
        Node::All(nodes) => nodes.iter().all(|n| eval(n, event)),
        Node::Any(nodes) => nodes.iter().any(|n| eval(n, event)),
        Node::Not(inner) => !eval(inner, event),
        Node::Leaf { field, matcher } => match event.get(field) {
            // When the field is missing (or null) no operator matches;
            // to express "this field is absent" use not + exists
            None | Some(Value::Null) => false,
            Some(v) => matcher.test(v),
        },
    }
}

impl Matcher {
    fn test(&self, v: &Value) -> bool {
        match self {
            Matcher::Exists => true,
            Matcher::Eq(x) => loose_eq(v, x),
            Matcher::Ne(x) => !loose_eq(v, x),
            Matcher::In(list) => list.iter().any(|x| loose_eq(v, x)),
            Matcher::Prefix(s) => as_text(v).starts_with(s.as_str()),
            Matcher::Suffix(s) => as_text(v).ends_with(s.as_str()),
            Matcher::Contains(s) => as_text(v).contains(s.as_str()),
            Matcher::Regex(re) => re.is_match(&as_text(v)),
            Matcher::Gt(n) => as_num(v).is_some_and(|x| x > *n),
            Matcher::Gte(n) => as_num(v).is_some_and(|x| x >= *n),
            Matcher::Lt(n) => as_num(v).is_some_and(|x| x < *n),
            Matcher::Lte(n) => as_num(v).is_some_and(|x| x <= *n),
        }
    }
}

/// Loose equality: a number in the rule compares numerically (both "404" and 404 in the log match),
/// anything else compares as a string
fn loose_eq(field: &Value, expected: &Value) -> bool {
    match expected.as_f64() {
        Some(n) => as_num(field) == Some(n),
        None => as_text(field) == as_text(expected),
    }
}

fn as_text(v: &Value) -> Cow<'_, str> {
    match v {
        Value::String(s) => Cow::Borrowed(s),
        other => Cow::Owned(other.to_string()),
    }
}

fn as_num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Build the one-line text for people: `[httpdog] <id> | <message> | <hits>x in <window>s | <group_by>=<value>`
fn format_alert(rule: &Rule, message: &str, group: &str, hits: usize) -> String {
    let mut text = format!("[httpdog] {} | {}", rule.id, message);
    if let Some((_, window)) = rule.threshold {
        text.push_str(&format!(" | {}x in {}s", hits, window.as_secs()));
    }
    if let Some(field) = &rule.group_by {
        text.push_str(&format!(" | {field}={group}"));
    }
    one_line(text)
}

/// Keep the text on one line by writing control characters in escaped form (`\n`, `\u{1b}`, ...).
/// Field values come from the request, and a JSON log can carry a real newline (`"uri":"/a\nALERT ..."`):
/// printed as it is, the client could forge another line in the system log
fn one_line(text: String) -> String {
    if !text.chars().any(char::is_control) {
        return text;
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if c.is_control() {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

/// Minimal template: replace {field} with that field's value, or "-" when it is missing
fn render(tpl: &str, event: &Value) -> String {
    let mut out = String::with_capacity(tpl.len() + 32);
    let mut rest = tpl;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) => {
                match event.get(&after[..end]) {
                    Some(v) => out.push_str(&as_text(v)),
                    None => out.push('-'),
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn engine(yaml: &str) -> Engine {
        Engine::from_config(serde_yaml_ng::from_str(yaml).unwrap()).unwrap()
    }

    fn compile_err(yaml: &str) -> String {
        match serde_yaml_ng::from_str::<Config>(yaml) {
            Err(e) => e.to_string(),
            Ok(cfg) => Engine::from_config(cfg).err().expect("should have failed"),
        }
    }

    const BASE: &str = "notifiers: { out: { type: stderr } }\nrules:\n";

    #[test]
    fn nested_conditions_and_loose_numbers() {
        let mut e = engine(&format!(
            "{BASE}
  - id: r
    cooldown: 0s
    notify: [out]
    when:
      all:
        - {{ field: uri, op: prefix, value: /admin }}
        - any:
            - {{ field: status, op: in, value: [401, 403] }}
            - {{ field: request_time, op: gt, value: 2 }}
        - not: {{ field: remote_addr, op: eq, value: 10.0.0.1 }}
"
        ));
        let now = Instant::now();
        let hit = |e: &mut Engine, v: Value| e.process(&v, "", now).len();

        assert_eq!(
            hit(
                &mut e,
                json!({"uri": "/admin/x", "status": 403, "remote_addr": "1.2.3.4"})
            ),
            1
        );
        // A string status must match too
        assert_eq!(
            hit(
                &mut e,
                json!({"uri": "/admin/x", "status": "401", "remote_addr": "1.2.3.4"})
            ),
            1
        );
        // Second branch of any
        assert_eq!(
            hit(
                &mut e,
                json!({"uri": "/admin", "status": 200, "request_time": 2.5, "remote_addr": "1.2.3.4"})
            ),
            1
        );
        // Excluded by not
        assert_eq!(
            hit(
                &mut e,
                json!({"uri": "/admin/x", "status": 403, "remote_addr": "10.0.0.1"})
            ),
            0
        );
        // prefix does not match
        assert_eq!(
            hit(
                &mut e,
                json!({"uri": "/", "status": 403, "remote_addr": "1.2.3.4"})
            ),
            0
        );
        // A missing field never matches
        assert_eq!(
            hit(&mut e, json!({"status": 403, "remote_addr": "1.2.3.4"})),
            0
        );
    }

    #[test]
    fn threshold_counts_per_group_within_window() {
        let mut e = engine(&format!(
            "{BASE}
  - id: brute
    when: {{ field: uri, op: eq, value: /login }}
    group_by: remote_addr
    threshold: {{ count: 3, window: 10s }}
    cooldown: 0s
    notify: [out]
"
        ));
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let a = json!({"uri": "/login", "remote_addr": "1.1.1.1"});
        let b = json!({"uri": "/login", "remote_addr": "2.2.2.2"});

        assert_eq!(e.process(&a, "", at(0)).len(), 0);
        assert_eq!(e.process(&b, "", at(1)).len(), 0); // Another IP does not add to the count
        assert_eq!(e.process(&a, "", at(2)).len(), 0);
        let fired = e.process(&a, "", at(3));
        assert_eq!(fired.len(), 1);
        assert!(
            fired[0].text.contains("remote_addr=1.1.1.1"),
            "{}",
            fired[0].text
        );

        // Counting starts again after firing
        assert_eq!(e.process(&a, "", at(4)).len(), 0);
        // Old hits outside the window do not count: the two at 20 and 21 plus one much later is not 3
        assert_eq!(e.process(&a, "", at(20)).len(), 0);
        assert_eq!(e.process(&a, "", at(21)).len(), 0);
        assert_eq!(e.process(&a, "", at(40)).len(), 0);
    }

    #[test]
    fn cooldown_suppresses_repeats_per_group() {
        let mut e = engine(&format!(
            "{BASE}
  - id: env
    when: {{ field: uri, op: suffix, value: /.env }}
    group_by: remote_addr
    cooldown: 5m
    notify: [out]
"
        ));
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let a = json!({"uri": "/.env", "remote_addr": "1.1.1.1"});
        let b = json!({"uri": "/.env", "remote_addr": "2.2.2.2"});

        assert_eq!(e.process(&a, "", at(0)).len(), 1);
        assert_eq!(e.process(&a, "", at(10)).len(), 0); // In cooldown
        assert_eq!(e.process(&b, "", at(10)).len(), 1); // Another IP is not affected
        assert_eq!(e.process(&a, "", at(299)).len(), 0);
        assert_eq!(e.process(&a, "", at(300)).len(), 1); // Cooldown over
    }

    #[test]
    fn default_cooldown_is_one_minute() {
        let mut e = engine(&format!(
            "{BASE}
  - id: r
    when: {{ field: uri, op: exists }}
    notify: [out]
"
        ));
        let t0 = Instant::now();
        let ev = json!({"uri": "/"});
        assert_eq!(e.process(&ev, "", t0).len(), 1);
        assert_eq!(e.process(&ev, "", t0 + Duration::from_secs(59)).len(), 0);
        assert_eq!(e.process(&ev, "", t0 + Duration::from_secs(60)).len(), 1);
    }

    #[test]
    fn sweep_drops_expired_state_only() {
        let mut e = engine(&format!(
            "{BASE}
  - id: r
    when: {{ field: uri, op: exists }}
    group_by: remote_addr
    threshold: {{ count: 5, window: 10s }}
    cooldown: 30s
    notify: [out]
"
        ));
        let t0 = Instant::now();
        e.process(&json!({"uri": "/", "remote_addr": "1.1.1.1"}), "", t0);
        e.sweep(t0 + Duration::from_secs(5));
        assert_eq!(e.state.len(), 1, "counts inside the window must be kept");
        e.sweep(t0 + Duration::from_secs(11));
        assert_eq!(e.state.len(), 0, "expired state must be swept");
    }

    const OVERLOAD: &str = "notifiers: { out: { type: stderr } }\nrules: []\noverload:\n";

    #[test]
    fn overload_reports_drops_and_accumulates_during_cooldown() {
        let mut e = engine(&format!("{OVERLOAD}  notify: [out]\n  cooldown: 5m\n"));
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let text = |a: Option<Alert>| a.map(|a| a.text);

        assert!(e.report_dropped(0, at(0)).is_none(), "no drops, no alert");
        // The first drop is reported right away
        let first = text(e.report_dropped(7, at(1))).unwrap();
        assert!(
            first.contains("overload") && first.contains("dropped 7 "),
            "{first}"
        );
        // During the cooldown drops only accumulate
        assert!(e.report_dropped(3, at(10)).is_none());
        assert!(e.report_dropped(2, at(300)).is_none());
        // Cooldown over: the accumulated count is reported even with no new drops this round
        let second = text(e.report_dropped(0, at(301))).unwrap();
        assert!(second.contains("dropped 5 "), "{second}");
        // The count resets once reported
        assert!(e.report_dropped(0, at(700)).is_none());
    }

    #[test]
    fn overload_defaults_to_one_minute_and_is_off_when_absent() {
        let mut e = engine(&format!("{OVERLOAD}  notify: [out]\n"));
        let t0 = Instant::now();
        assert!(e.report_dropped(1, t0).is_some());
        assert!(e.report_dropped(1, t0 + Duration::from_secs(59)).is_none());
        assert!(e.report_dropped(0, t0 + Duration::from_secs(60)).is_some());

        // No overload section: drops are only logged, no alert
        let mut off = engine("notifiers: { out: { type: stderr } }\nrules: []\n");
        assert!(off.report_dropped(100, t0).is_none());
        assert!(
            off.report_dropped(100, t0 + Duration::from_secs(3600))
                .is_none()
        );
    }

    #[test]
    fn combined_format_checks_field_names_at_load() {
        let cfg =
            |rule: &str| format!("format: combined\n{BASE}  - id: r\n    notify: [out]\n{rule}");

        // Fields combined has: conditions (nested too) and group_by all pass
        let e = engine(&cfg(
            "    group_by: remote_addr\n    when:\n      all:\n        - { field: uri, op: prefix, value: /login }\n        - not: { field: status, op: lt, value: 400 }\n",
        ));
        assert_eq!(e.format(), Format::Combined);

        // combined has no host and no request_time
        let err = compile_err(&cfg(
            "    when: { field: host, op: eq, value: example.com }\n",
        ));
        assert!(err.contains("host") && err.contains("combined"), "{err}");
        // Caught even when hidden in a nested condition
        let err = compile_err(&cfg(
            "    when:\n      any:\n        - { field: uri, op: exists }\n        - not: { field: request_time, op: gt, value: 5 }\n",
        ));
        assert!(err.contains("request_time"), "{err}");
        // A typo
        let err = compile_err(&cfg("    when: { field: stauts, op: eq, value: 404 }\n"));
        assert!(err.contains("stauts"), "{err}");
        // group_by is checked too
        let err = compile_err(&cfg(
            "    group_by: host\n    when: { field: status, op: gte, value: 500 }\n",
        ));
        assert!(err.contains("group_by") && err.contains("host"), "{err}");

        // No format (json): the fields come from log_format and are not checked
        let e = engine(&format!(
            "{BASE}  - id: r\n    notify: [out]\n    when: {{ field: anything_goes, op: exists }}\n"
        ));
        assert_eq!(e.format(), Format::Json);
        // An unknown format value
        compile_err(&format!("format: clf\n{BASE}"));
    }

    #[test]
    fn alerts_carry_structured_details() {
        let mut e = engine(&format!(
            "{BASE}
  - id: brute
    when: {{ field: uri, op: eq, value: /login }}
    group_by: remote_addr
    threshold: {{ count: 2, window: 30s }}
    notify: [out]
    message: \"{{remote_addr}} repeated login failures\"
  - id: plain
    when: {{ field: status, op: eq, value: 500 }}
    cooldown: 0s
    notify: [out]
"
        ));
        let t0 = Instant::now();
        let ev = json!({"uri": "/login", "remote_addr": "1.1.1.1", "status": 401});
        assert!(e.process(&ev, "", t0).is_empty());
        let fired = e.process(&ev, "", t0);
        let d = &fired[0].detail;
        assert!(matches!(d.kind, Kind::Rule));
        assert_eq!(d.rule, "brute");
        // message holds only the message itself; the prefix, count and group appear only in text
        assert_eq!(d.message, "1.1.1.1 repeated login failures");
        assert_eq!(
            fired[0].text,
            "[httpdog] brute | 1.1.1.1 repeated login failures | 2x in 30s | remote_addr=1.1.1.1"
        );
        assert_eq!(d.group, Some(("remote_addr".into(), "1.1.1.1".into())));
        assert_eq!(d.threshold, Some((2, 30)));
        assert_eq!(d.dropped, None);
        assert_eq!(d.event.as_ref(), Some(&ev));

        // A rule with no group_by / threshold / message: message is the start of the raw log line
        let raw = r#"{"status":500}"#;
        let fired = e.process(&json!({"status": 500}), raw, t0);
        let d = &fired[0].detail;
        assert_eq!(d.rule, "plain");
        assert_eq!(d.message, raw);
        assert_eq!(d.group, None);
        assert_eq!(d.threshold, None);
    }

    #[test]
    fn alert_text_stays_on_one_line() {
        let now = Instant::now();
        let mut e = engine(&format!(
            "{BASE}  - id: r\n    when: {{ field: uri, op: prefix, value: /x }}\n    group_by: uri\n    notify: [out]\n    message: \"hit {{uri}}\"\n"
        ));
        // A JSON log line can carry a real newline in a value; it must not start a new line in the log
        let ev = json!({"uri": "/x\nALERT [httpdog] forged | \u{1b}[31m\r"});
        let a = e.process(&ev, "", now).remove(0);
        assert!(!a.text.chars().any(char::is_control), "{:?}", a.text);
        assert!(
            a.text
                .contains(r"hit /x\nALERT [httpdog] forged | \u{1b}[31m\r"),
            "{}",
            a.text
        );
        // The structured parts keep the value as it is; JSON encoding keeps them safe in a webhook
        assert_eq!(
            a.detail.message,
            "hit /x\nALERT [httpdog] forged | \u{1b}[31m\r"
        );

        // The raw log preview (a rule without a message) is covered as well
        let mut e = engine(&format!(
            "{BASE}  - id: r\n    when: {{ field: a, op: exists }}\n    notify: [out]\n"
        ));
        let a = e
            .process(&json!({"a": 1}), "{\"a\":1,\n\"b\":2}", now)
            .remove(0);
        assert!(!a.text.chars().any(char::is_control), "{:?}", a.text);
    }

    #[test]
    fn overload_alert_carries_the_dropped_count() {
        let mut e = engine(&format!("{OVERLOAD}  notify: [out]\n"));
        let a = e.report_dropped(42, Instant::now()).unwrap();
        assert!(matches!(a.detail.kind, Kind::Overload));
        assert_eq!(a.detail.rule, "overload");
        assert_eq!(a.detail.dropped, Some(42));
        assert_eq!(a.detail.message, "falling behind, dropped 42 log lines");
        assert_eq!(
            a.text,
            "[httpdog] overload | falling behind, dropped 42 log lines"
        );
        assert!(a.detail.event.is_none());
    }

    #[test]
    fn instance_travels_with_every_alert() {
        let rule = "  - id: r\n    notify: [out]\n    when: { field: a, op: exists }\n";
        let overload = "overload:\n  notify: [out]\n";
        let fire = |yaml: &str| {
            let mut e = engine(yaml);
            let now = Instant::now();
            let rule_alert = e.process(&json!({"a": 1}), "{}", now).remove(0);
            let overload_alert = e.report_dropped(3, now).unwrap();
            (rule_alert, overload_alert)
        };

        let (r, o) = fire(&format!("instance: web-01\n{BASE}{rule}{overload}"));
        assert_eq!(&*r.instance, "web-01");
        assert_eq!(&*o.instance, "web-01");
        // The name is not mixed into text: it is a separate field in the webhook body
        assert!(r.text.starts_with("[httpdog] r |"), "{}", r.text);

        // unknown when not set
        let (r, o) = fire(&format!("{BASE}{rule}{overload}"));
        assert_eq!(&*r.instance, "unknown");
        assert_eq!(&*o.instance, "unknown");
    }

    #[test]
    fn webhook_has_no_payload_setting() {
        // The webhook body has a single shape and the payload key is gone; a config that still has it must fail at load time
        // rather than be silently ignored (payload: text above all: the receiver would expect the old format)
        for value in ["text", "structured"] {
            let err = compile_err(&format!(
                "notifiers: {{ w: {{ type: webhook, url: 'http://x', secret: k, payload: {value} }} }}\nrules: []\n"
            ));
            assert!(err.contains("payload"), "{value}: {err}");
        }
    }

    #[test]
    fn webhook_requires_a_secret() {
        let cfg = |extra: &str| {
            format!(
                "notifiers: {{ w: {{ type: webhook, url: 'http://x'{extra} }} }}\nrules:\n  - id: r\n    notify: [w]\n    when: {{ field: a, op: exists }}\n"
            )
        };
        let e = engine(&cfg(", secret: abc123"));
        match &e.rules[0].targets[0] {
            Target::Webhook { secret, .. } => assert_eq!(secret, "abc123"),
            Target::Stderr => panic!("should be a webhook"),
        }

        // No secret, no load
        assert!(compile_err(&cfg("")).contains("secret_env"));
        // Even when no rule uses this notifier
        let unused = "notifiers: { w: { type: webhook, url: 'http://x' } }\nrules: []\n";
        assert!(compile_err(unused).contains("secret_env"));

        assert!(compile_err(&cfg(", secret: a, secret_env: X")).contains("only one"));
        assert!(compile_err(&cfg(", secret: ''")).contains("empty"));
        // Unset environment variable: must fail at load time, and the message must name the variable
        let err = compile_err(&cfg(", secret_env: HTTPDOG_TEST_SECRET_THAT_IS_NOT_SET"));
        assert!(err.contains("HTTPDOG_TEST_SECRET_THAT_IS_NOT_SET"), "{err}");
    }

    #[test]
    fn message_template() {
        let ev = json!({"uri": "/a", "status": 500});
        assert_eq!(
            render("{status} on {uri} from {nope}", &ev),
            "500 on /a from -"
        );
        assert_eq!(render("no placeholders", &ev), "no placeholders");
        assert_eq!(render("dangling {uri", &ev), "dangling {uri");
    }

    #[test]
    fn bad_configs_are_rejected_at_load() {
        let rule = |body: &str| format!("{BASE}  - id: r\n    notify: [out]\n    when: {body}\n");

        assert!(compile_err(&rule("{ field: uri, op: regex, value: '(' }")).contains("regex"));
        assert!(compile_err(&rule("{ field: status, op: gt, value: abc }")).contains("number"));
        assert!(compile_err(&rule("{ field: status, op: in, value: 5 }")).contains("list"));
        // A mistyped operator name
        compile_err(&rule("{ field: uri, op: startswith, value: / }"));
        // An extra unknown key
        compile_err(&rule("{ field: uri, op: eq, value: /, oops: 1 }"));

        let unknown_notifier =
            format!("{BASE}  - id: r\n    notify: [nope]\n    when: {{ field: a, op: exists }}\n");
        assert!(compile_err(&unknown_notifier).contains("nope"));

        let dup = format!(
            "{BASE}  - id: r\n    notify: [out]\n    when: {{ field: a, op: exists }}\n  - id: r\n    notify: [out]\n    when: {{ field: a, op: exists }}\n"
        );
        assert!(compile_err(&dup).contains("duplicate"));

        let both = "notifiers: { w: { type: webhook, url: 'http://x', url_env: X, secret: k } }\nrules: []\n";
        assert!(compile_err(both).contains("exactly one"));

        // instance: empty, too long and control characters are all rejected
        for bad in ["''", "'   '", "\"a\\nb\"", &"x".repeat(65)] {
            let err = compile_err(&format!("instance: {bad}\n{BASE}  []\n"));
            assert!(err.contains("instance"), "{bad:?}: {err}");
        }
        engine(&format!("instance: {}\n{BASE}  []\n", "x".repeat(64)));
        engine(&format!("instance: zürich-web-01\n{BASE}  []\n"));

        // overload goes through the same notifier checks as rules
        let unknown = compile_err(&format!("{OVERLOAD}  notify: [nope]\n"));
        assert!(
            unknown.contains("overload") && unknown.contains("nope"),
            "{unknown}"
        );
        assert!(compile_err(&format!("{OVERLOAD}  notify: []\n")).contains("empty"));
        compile_err(&format!("{OVERLOAD}  cooldown: 1m\n")); // notify is missing
        compile_err(&format!("{OVERLOAD}  notify: [out]\n  oops: 1\n")); // An unknown key
        compile_err(&format!("{OVERLOAD}  notify: [out]\n  cooldown: 5 days\n"));
    }
}
