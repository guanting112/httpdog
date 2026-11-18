# Changelog

Notable changes in each version. The release workflow uses the section for a version as its release notes, so add a section here before releasing.

## [Unreleased]

### Changed

- Every release also carries its build provenance as `httpdog.intoto.jsonl`, the same content as `httpdog.sigstore.json` under the name OpenSSF Scorecard looks for. The file was added to the five releases already published; their binaries and other files are unchanged.

### Documentation

- The README and `docs/webhook.md` point to [httpdog-house](https://github.com/guanting112/httpdog-house), a separate project that receives httpdog's webhook alerts, verifies them, and keeps them searchable on web pages, on your own Cloudflare account. The README shows a screenshot of its alerts page, in the reader's light or dark theme.

## [0.2.3] - 2026-11-16

The program behaves the same as 0.2.2. The changes are in the systemd unit file, the documentation, and a guarantee about the code.

### Added

- `#![forbid(unsafe_code)]` in the library and the binary: the compiler now refuses `unsafe` code in httpdog itself. There was none; this keeps it that way.

### Documentation

- The README says what httpdog being written in Rust buys: speed, with the measured rate (250,000 to 500,000 lines a second on a laptop, depending on the rules), and memory safety for a program that reads text chosen by attackers. `docs/limits.md` has the measurement.

### Changed

- `deploy/httpdog.service` puts a ceiling of 256 MB on httpdog's memory (`MemoryMax=256M`, `MemorySwapMax=0`). httpdog normally uses a few megabytes, but the counters of a rule with `group_by` grow with the number of distinct groups, and nothing limited that. At the ceiling the kernel kills httpdog and systemd restarts it with empty counters. To get it, install the new unit file and run `sudo systemctl daemon-reload && sudo systemctl restart httpdog`.

## [0.2.2] - 2026-11-16

### Fixed

- Memory was not given back after a burst of distinct groups. A rule with `group_by` keeps a counter for every group it is counting. The counters were removed once they expired, but the table that held them stayed at its largest size until the next restart: 500,000 groups left about 80 MB behind. The table now shrinks when it empties.

## [0.2.1] - 2026-11-16

Fixes two problems found by installing 0.2.0 on servers at a cloud provider. Each could leave a site without alerts it should have had.

### Fixed

- **The example rules missed probes on sites with a custom error page or a front controller.** They matched paths with `uri`, which nginx replaces: with `error_page 404 /404.html;` (the default configuration of Rocky Linux 9 has it) a request for `/.env` that ends in a 404 is logged with `uri` set to `/404.html`, and with `try_files ... /index.php` every request has `uri` set to `/index.php`. `rules.example.yaml` and the templates under `examples/` now match paths with the new `request_path`. `sensitive-file-exposed` still matches `uri` on purpose: it is about the file nginx actually served. Rules of your own that match `uri` behave as before; switch them to `request_path` if your site has either of the two.
- **With SELinux enforcing, nginx could not send its log to the unix socket**, so no alert was ever raised. This is the default state of Fedora, RHEL and Rocky Linux. The new `deploy/httpdog-selinux.cil` holds the one rule that allows it; install it with `sudo semodule -i httpdog-selinux.cil`.

### Added

- `request_path`: the path the client asked for, which is `request_uri` without the query string. With the JSON format httpdog adds it to every log line that has `request_uri`, so nginx's `log_format` needs no change; the `combined` format has it too, with the same value as `uri`. The webhook body carries it in `event`.

### Documentation

- `docs/log-format.md` explains the three path fields (`request_path`, `request_uri`, `uri`) and when to use which.
- `docs/limits.md` lists the six cloud servers 0.2.0 was installed on, including two with SELinux enforcing.

## [0.2.0] - 2026-11-16

### Changed (incompatible)

- Logs now arrive on a unix socket by default: `listen` defaults to `unix:/run/httpdog/httpdog.sock` instead of the UDP address `127.0.0.1:5514`. httpdog believes every line it receives, and on UDP any program that could reach the address could send lines and raise false alerts. The socket is open to root and the web server's group only. To upgrade from 0.1.0: install the new `deploy/httpdog.service`, change nginx's `syslog:server=` to `unix:/run/httpdog/httpdog.sock`, restart httpdog and reload nginx. A rule file that sets `listen: 127.0.0.1:5514` itself, as the `rules.example.yaml` of 0.1.0 did, keeps receiving over UDP and now prints the warning below.
- A malformed `listen` is now a rule file error (exit status 78) instead of a failure to listen (exit status 1).

### Added

- `listen: unix:/path` receives logs on a unix datagram socket, created with mode `0660`. `deploy/httpdog.service` hands it to the group nginx's workers run as; httpdog does not join that group.
- A warning in the log at every start when `listen` is a UDP address, with stronger wording when the address is not a loopback address.
- A reload that finds a changed `listen` says that it only takes effect after a restart.

### Known limitations

- Restarting httpdog without reloading nginx loses one log line per nginx worker, see `docs/limits.md`.

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

[Unreleased]: https://github.com/guanting112/httpdog/compare/v0.2.3...HEAD
[0.2.3]: https://github.com/guanting112/httpdog/releases/tag/v0.2.3
[0.2.2]: https://github.com/guanting112/httpdog/releases/tag/v0.2.2
[0.2.1]: https://github.com/guanting112/httpdog/releases/tag/v0.2.1
[0.2.0]: https://github.com/guanting112/httpdog/releases/tag/v0.2.0
[0.1.0]: https://github.com/guanting112/httpdog/releases/tag/v0.1.0
