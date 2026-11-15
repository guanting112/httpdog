# Changelog

Notable changes in each version. The release workflow uses the section for a version as its release notes, so add a section here before releasing.

## [Unreleased]

## [0.1.0] - 2026-11-15

First release.

### Features

- Reads nginx / Apache access logs, matches them against a rule file and sends alerts. Input arrives over syslog UDP, or through `--stdin` for pipes and for replaying old logs.
- Supports the `json` log format (with nginx's `log_format ... escape=json`) and `combined` (the default format of nginx / Apache, so the web server's log settings stay untouched; Apache's `common` works too).
- Rules are written in YAML or JSON. Conditions nest freely with `all` / `any` / `not`, with 12 operators (`eq`, `ne`, `in`, `prefix`, `suffix`, `contains`, `regex`, `gt`, `gte`, `lt`, `lte`, `exists`).
- Thresholds (`threshold`: fire only after M hits within N seconds), grouping (`group_by`: for example each IP counted separately) and cooldowns (`cooldown`: notify at most once per group within a period).
- Notifies stderr or a webhook. A webhook POSTs JSON to your own receiver, with a separate field for each part of the alert. A failure is retried, for 3 attempts in total.
- Webhook signatures: every webhook must have a secret, and every request carries `X-Httpdog-Timestamp` and `X-Httpdog-Signature` (HMAC-SHA256), so the receiver can verify that the request really came from httpdog, that the body was not altered, and reject expired requests.
- `instance` at the top of the rule file: the name of this httpdog instance (e.g. `web-01`). The webhook body carries it, which tells the sources apart when several send to the same receiver.
- Log content is treated as hostile: the one-line text of an alert never contains a control character, so a request cannot forge a line in the system log. The log parsers and the rule engine are fuzzed.
- Release binaries come with a signed build provenance attestation (`gh attestation verify`).
- `overload`: when logs are dropped because traffic exceeds the processing rate, an alert can be sent in addition to the log line.
- The rule file is validated completely at load time, and a mistake is reported with its location. `--check` validates without starting.
- Built for systemd: `Type=notify`, watchdog, reload on `SIGHUP` (a broken new rule file keeps the old rules), and exit status 78 on a rule file error so it is not restarted forever.
- Ships 6 example rule files (`examples/`): a starter for the `combined` format, plus 5 scenarios: security probing, login abuse, site health, traffic anomalies and business-critical paths.
- Replaying `sample.log` with `rules.example.yaml` sends 3 alerts, so it can be used to test your own receiver.

### Known limitations

- Thresholds use the time a log is received, not the time written in the log.
- Counters and cooldowns are in memory only and reset on restart or reload.
- There is no built-in file-reading mode; reading a log file needs `tail -F` piped into `--stdin`.
- Verified under systemd with real nginx and Apache on Ubuntu 22.04 to 26.04, Debian 11 to 13, Fedora 44 and Rocky Linux 10, but not yet under production traffic. See `docs/limits.md` for the full list and the verification status.

[Unreleased]: https://github.com/guanting112/httpdog/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/guanting112/httpdog/releases/tag/v0.1.0
