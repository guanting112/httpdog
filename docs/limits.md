# Known limitations and verification status

## Known limitations

httpdog is kept simple on purpose. These are the current trade-offs.

**Counting and state**

- **Thresholds use the time a log is received**, not the time inside the log. It makes no difference for live monitoring, but when old logs are replayed with `--stdin` every line arrives at once, so threshold rules fire more easily than they really would have.
- **State is in memory only** and is lost on restart, so the same thing may be notified again after a restart.
- **A reload resets counters and cooldowns**, including the dropped count that `overload` accumulated during a cooldown and has not reported yet.

**Receiving logs**

- **Logs are dropped when traffic exceeds the processing rate**: at most 8192 lines wait to be processed, and new lines are dropped once that is full, so memory cannot grow without bound. Dropped lines are not matched against rules, so threshold counts are too low for that period. To be notified when it happens, see [overload](rules.md#overload-alerts-when-falling-behind).
- **syslog goes over UDP**: nginx's syslog output does not support TCP. A very long request may be truncated, and the truncated JSON line fails to parse and is skipped. UDP packets may also be lost at traffic peaks.
- **Only the `json` and `combined` log formats are supported.** Custom non-JSON formats and Apache's `vhost_combined` (which starts with the site name) are not.
- **There is no built-in file-reading mode.** Reading a log file needs `tail -F` piped into `--stdin`, and that setup has no matching systemd unit yet.

**Rules**

- **No CIDR matching** (such as "source within 10.0.0.0/8"), and two fields cannot be compared with each other.

**Notifications**

- **A webhook has one body and it is meant for programs**; there are no custom templates. Incoming webhooks of chat tools each have their own format and cannot receive it directly. To send there, put a receiver of your own in between and convert to what the other side expects.
- **The signature only helps if the receiver verifies it.** Every webhook request is signed, but when the receiver does not check, security still rests on keeping the URL secret.

## Verification status

Which parts have actually been run, and which have not.

**Linux**

- Tested: unit tests, real UDP syslog packets, webhook retries on failure, reload (including a broken rule file), draining on SIGTERM, systemd notify messages.
- GitHub Actions runs on Ubuntu 24.04 on every push (`.github/workflows/build.yml`): every check in `make ci`, then a musl static build on an amd64 runner and an arm64 runner, and the rule file checks and the smoke tests on both sample logs again with the built binaries. This covers the stdin path for both `json` and `combined`.
- Not yet tested on Linux: `overload` alerts, the webhook body and signature, `instance`, and `combined` over UDP or an Apache pipe.

**macOS (Apple Silicon, the development machine)**

- Tested: build, clippy, unit tests, smoke tests on both sample logs.
- `overload` alerts: flooded over UDP beyond the processing rate and confirmed that both the system log and the webhook received them, with a dropped count matching the actual number.
- Reload: tested that a broken new rule file keeps the old rules. A successful reload onto new rules and retries against an unreachable webhook have not been tested on macOS.

**The `combined` format**

- Tested with the Apache 2.4 bundled with macOS: `combined` and `common` logs it actually produced (including escaped quotes, `-` and invalid request lines) all parsed, and both the direct `CustomLog` pipe and UDP with a syslog header worked.
- `combined` written by nginx (`\xHH` escapes) only has unit tests; it has not been verified against logs from a real nginx.

**webhook**

- Body: tested with a local receiver. The bodies sent for rule alerts and `overload` alerts, in both the `json` and `combined` formats, matched the documentation.
- Signature: tested with the Python receiver in [webhook.md](webhook.md#receiver-example). Requests signed with the right secret were accepted (including non-ASCII content); requests with a wrong secret, no signature or an expired timestamp were all rejected. The algorithm also has a unit test that compares against a value computed independently in Python.
- "Will not start without a secret": at startup httpdog exits with status 78; a reload onto a rule file without a secret keeps the old rules and goes on sending signed requests.
- `instance`: the `instance` field in the body was correct both when set and when not set (`unknown`), and the signature verified. The `instance` carried by `overload` alerts only has unit tests; changing the name on reload has not been tested.
- It has not been connected to a real external service.

**Fuzzing**

- The four targets under `fuzz/fuzz_targets` (the two log parsers, the rule engine, rule file loading) were run on macOS with cargo-fuzz, five minutes each (about 7.9 million runs in total), without a crash or a failed assertion.
- Writing the targets turned up one real problem, fixed before the first release: with the JSON format, a newline inside a field value reached the alert text as it was, so a request could add a forged line to the system log. The alert text is now always a single line, and the `rule_engine` target asserts it.
- The fuzz workflow (`.github/workflows/fuzz.yml`) has not run on GitHub Actions yet.

**Not verified at all**

- Running the unit file under a real systemd.
- A real nginx. The logs used in tests were written by hand in the format of `deploy/nginx-snippet.conf`.
