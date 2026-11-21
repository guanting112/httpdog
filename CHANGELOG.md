# Changelog

Notable changes in each version. The release workflow uses the section for a version as its release notes, so add a section here before releasing.

## [Unreleased]

### Added

- A seventh rule template, `examples/06-private-data.yaml`, for private files and customer data: guessing the names of uploaded files (ID scans, contracts, spreadsheets), bulk downloads of documents, walking through record numbers, one address opening member records or orders in the admin area in bulk, and every export from the admin area.
- The README shows how to have a coding agent (Claude Code, OpenAI Codex) write the rules from your project's code, with a prompt to start from.

### Changed

- `rules.example.yaml` and the templates under `examples/` carry far fewer comments, so they are easier to edit. What the comments explained is in `docs/rules.md`, which each file links to; what to change in each template is in a table under "Using the rule templates".

## [0.3.0] - 2026-11-21

Restarting httpdog no longer loses log lines, and `sudo httpdog -t` checks the rule file the way `nginx -t` checks nginx's configuration. The first of these changes how httpdog is installed under systemd: there are now two unit files, and an existing install has to be upgraded by hand, see "Two unit files in place of one" below.

### Added

- `httpdog -t` checks the rule file the way `nginx -t` checks nginx's configuration. `-t` is `--check` under a shorter name, with two things that make `sudo httpdog -t` enough on an installed host. Without `--config` it checks `/etc/httpdog/rules.yaml`. And a variable the rule file refers to that is not in the environment is looked up in `/etc/httpdog/secrets.env`, read the way systemd reads it, so the check no longer ends in `environment variable ... is not set` for lack of the webhook URL and secret. The answer names the file: `OK: /etc/httpdog/rules.yaml: 4 rule(s)`.

### Fixed

- A restart of httpdog lost one log line for every nginx worker, and every line sent while it was down. httpdog created its socket itself, so each restart made a new one: nginx's workers were still connected to the old one, and each found out by losing its next line. Unless nginx was reloaded right after, a probe that happened to be a worker's first request after the restart raised no alert. Now systemd creates the socket and holds it (`deploy/httpdog.socket`), and passes it to httpdog at every start. A restart keeps the same socket, nginx needs no reload, and the lines sent in the meantime wait in the socket until httpdog is back: a few hundred fit, 512 at most. This covers `systemctl restart`, a crash, the watchdog and the memory ceiling.

### Changed

- **Two unit files in place of one.** `deploy/httpdog.socket` is new and describes the socket: its path, its mode (`0660`) and its group. `deploy/httpdog.service` no longer creates the directory or hands the socket to the web server's group, and needs `httpdog.socket`. Who can send logs is unchanged, root and the web server's group, and httpdog is still not a member of that group; it now cannot remove or replace the socket file either, which belongs to root. To upgrade an install of 0.2.x:

  ```bash
  sudo systemctl stop httpdog            # While the old unit file is still loaded: it removes the old socket
  # Install the new binary (step 1 of the README), fetch the two unit files (step 2), then:
  sudo install -m 644 httpdog.service httpdog.socket /etc/systemd/system/
  sudo systemctl daemon-reload
  sudo systemctl enable --now httpdog    # Enables and starts httpdog.socket too
  sudo systemctl reload nginx            # One last time
  ```

  The new binary also runs under the old unit file, behaving as before. The new unit files need the new binary: an older one fails with `a socket is already there and cannot be checked`.
- **The group is set in `httpdog.socket`.** If you changed `www-data` in `httpdog.service` (to `nginx` on Fedora or Rocky Linux), make the same change to `SocketGroup=` in `httpdog.socket`.
- **With a `listen` other than the default, the address goes in `httpdog.socket` as well.** httpdog checks that the socket systemd passed is the one `listen` names and does not start when they differ (exit status 78), because it would otherwise run and receive nothing. For UDP, write `ListenDatagram=127.0.0.1:5514` in `httpdog.socket`. With the default socket there is nothing to do. For the same reason a reload refuses a rule file whose `listen` differs from the socket systemd holds, and keeps the old rules: taking it would leave a rule file that works until the next restart or reboot and then keeps httpdog down.
- **Stopping httpdog alone does not keep it stopped.** The socket stays with systemd, and the next log line starts httpdog again. Use `sudo systemctl stop httpdog.socket httpdog`.
- **After a rule file error at startup** (exit status 78), systemd tries a few more times when a log line arrives and then stops `httpdog.socket` too. Fix the file, then `sudo systemctl start httpdog && sudo systemctl reload nginx`.
- httpdog started without systemd, or without `httpdog.socket`, creates its socket itself as before.
- `--check` without `--config` now checks `/etc/httpdog/rules.yaml`, where it used to check `./rules.yaml`, and its answer starts with the name of the file. Running httpdog without `--config` still uses `./rules.yaml`.
- A new dependency, `sd-listen-fds` (142 lines, no dependencies of its own), takes the socket systemd passes. That step needs `unsafe` in Rust, which httpdog's own code still forbids.
- Every release also carries its build provenance as `httpdog.intoto.jsonl`, the same content as `httpdog.sigstore.json` under the name OpenSSF Scorecard looks for. The file was added to the five releases already published; their binaries and other files are unchanged.

### Documentation

- The README starts with the install, and the install is one path from the download to a first alert: it now includes connecting nginx (the snippet goes into `/etc/nginx/conf.d/`), a placeholder for whoever has no receiver yet, and reading httpdog's log. The "Try it first" section is gone; replaying the sample log to test a receiver is in `docs/webhook.md`.
- New in the README and `docs/log-format.md`: a `server { }` block with an `access_log` line of its own does not use the one for httpdog, and needs the line as well.
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

[Unreleased]: https://github.com/guanting112/httpdog/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/guanting112/httpdog/releases/tag/v0.3.0
[0.2.3]: https://github.com/guanting112/httpdog/releases/tag/v0.2.3
[0.2.2]: https://github.com/guanting112/httpdog/releases/tag/v0.2.2
[0.2.1]: https://github.com/guanting112/httpdog/releases/tag/v0.2.1
[0.2.0]: https://github.com/guanting112/httpdog/releases/tag/v0.2.0
[0.1.0]: https://github.com/guanting112/httpdog/releases/tag/v0.1.0
