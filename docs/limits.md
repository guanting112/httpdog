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
- **Notifications are sent one at a time.** While a webhook is failing, each alert waits out its retries (up to about 18 seconds) before the next one is handled, and that includes the alert lines written to the system log. Matching is not affected.
- **The signature only helps if the receiver verifies it.** Every webhook request is signed, but when the receiver does not check, security still rests on keeping the URL secret.

## Verification status

Which parts have actually been run, and which have not.

**Linux, under a real systemd with a real nginx and Apache**

Tested on 2026-11-15 with the static binaries built by CI, in OrbStack machines on an Apple Silicon Mac. Each machine runs its distribution's own systemd, nginx and Apache. The kernel is OrbStack's, shared by all machines, not the distribution's own, and SELinux was disabled on Fedora and Rocky Linux.

| Distribution | Architecture | systemd | nginx | Apache | Checks passed |
|---|---|---|---|---|---|
| Ubuntu 22.04 | arm64 | 249 | 1.18.0 | 2.4.52 | 53 of 53 |
| Ubuntu 24.04 | arm64 | 255 | 1.24.0 | 2.4.58 | 53 of 53 |
| Ubuntu 24.04 | amd64 (emulated) | 255 | 1.24.0 | 2.4.58 | 53 of 53 |
| Ubuntu 26.04 | arm64 | 259 | 1.28.3 | 2.4.66 | 53 of 53 |
| Debian 11 | arm64 | 247 | 1.18.0 | 2.4.67 | 53 of 53 |
| Debian 12 | arm64 | 252 | 1.22.1 | 2.4.71 | 53 of 53 |
| Debian 13 | arm64 | 257 | 1.26.3 | 2.4.71 | 53 of 53 |
| Fedora 44 | arm64 | 259 | 1.30.6 | not tested | 50 of 50 |
| Rocky Linux 10 | arm64 | 257 | 1.26.3 | not tested | 50 of 50 |

What was checked on every machine:

- **The binary**: it runs and is statically linked, the example rule files pass `--check`, and both smoke tests fire the expected number of alerts.
- **systemd**, installed the way the README says: the unit becomes active through `Type=notify`; the process does not run as root (`DynamicUser`); a reload with a broken rule file keeps the old rules, and one with a good file applies the new rules and a new `instance` name; the service is restarted after `SIGKILL`; the watchdog restarts a frozen process (`SIGSTOP`) within 45 seconds; `systemctl stop` flushes and exits cleanly; a broken rule file or a missing secret ends with exit status 78 and is not restarted; the secret never appears in the journal.
- **nginx, JSON format**, with `deploy/nginx-snippet.conf` and real requests: a single-hit rule, a threshold rule, nginx's fields in the webhook body, and a request with an encoded newline in the path, which arrives escaped and cannot add a line to the journal.
- **nginx, `combined` format**, with `examples/00-combined-starter.yaml`, including a User-Agent with a quote (nginx's `\x22` escape).
- **webhook**: the receiver from [webhook.md](webhook.md#receiver-example) accepts httpdog's requests and rejects a wrong signature; `instance` is correct; an `overload` alert is raised and delivered when UDP is flooded against a deliberately slow rule file.
- **Apache, `combined` format** (Debian and Ubuntu only): both the `CustomLog` pipe and `tail -F` piped into `--stdin`.

GitHub Actions also runs on Ubuntu 24.04 on every push (`.github/workflows/build.yml`): every check in `make ci`, then a musl static build on an amd64 runner and an arm64 runner, and the rule file checks and smoke tests again with the built binaries.

**macOS (Apple Silicon, the development machine)**

- Tested: build, clippy, unit tests, smoke tests on both sample logs.
- `overload` alerts, reload with a broken rule file, the webhook body and signature, and `combined` logs from the Apache 2.4 bundled with macOS were also tested here before the Linux run above.

**Fuzzing**

- The four targets under `fuzz/fuzz_targets` (the two log parsers, the rule engine, rule file loading) were run on macOS with cargo-fuzz, five minutes each (about 7.9 million runs in total), without a crash or a failed assertion.
- Writing the targets turned up one real problem, fixed before the first release: with the JSON format, a newline inside a field value reached the alert text as it was, so a request could add a forged line to the system log. The alert text is now always a single line, and the `rule_engine` target asserts it.
- The fuzz workflow (`.github/workflows/fuzz.yml`) runs the same targets on GitHub Actions (Ubuntu 24.04): a minute each on every push, 15 minutes each once a week.

**Not verified**

- The distributions' own kernels, and SELinux in enforcing mode.
- amd64 on real hardware under systemd (the amd64 machine above was emulated; CI runs the amd64 binary's smoke tests on a real amd64 runner).
- Long-running operation and real production traffic.
- A real external service as the webhook receiver.
