# Known limitations and verification status

## Known limitations

httpdog is kept simple on purpose. These are the current trade-offs.

**Counting and state**

- **Thresholds use the time a log is received**, not the time inside the log. It makes no difference for live monitoring, but when old logs are replayed with `--stdin` every line arrives at once, so threshold rules fire more easily than they really would have.
- **State is in memory only** and is lost on restart, so the same thing may be notified again after a restart.
- **A reload resets counters and cooldowns**, including the dropped count that `overload` accumulated during a cooldown and has not reported yet.

**Receiving logs**

- **Logs are dropped when traffic exceeds the processing rate**: at most 8192 lines wait to be processed, and new lines are dropped once that is full, so memory cannot grow without bound. Dropped lines are not matched against rules, so threshold counts are too low for that period. To be notified when it happens, see [overload](rules.md#overload-alerts-when-falling-behind).
- **httpdog believes every line it receives.** It cannot tell a line the web server wrote from one another program made up. The default unix socket narrows the senders to root and the web server's group, which still includes a web application running under the web server's account. With a UDP `listen`, anything that can reach the address can send. See [How logs reach httpdog](log-format.md#how-logs-reach-httpdog).
- **Restarting httpdog alone loses one line per nginx worker.** The restart creates a new socket; each worker notices on its next log line, which is lost, and reconnects for the one after. Reloading nginx right after the restart avoids it. Lines sent while httpdog is down are lost either way.
- **syslog is sent as datagrams** (over the unix socket, or UDP): nginx's syslog output does not support TCP. A very long request may be truncated, and the truncated JSON line fails to parse and is skipped. Datagrams may also be lost at traffic peaks.
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

The runs above were made with 0.1.0, where logs arrived over UDP on `127.0.0.1:5514`.

**The unix socket input**

Added in 0.2.0 and tested on 2026-11-15 in the same kind of machines, installed the way the README says, with nginx's `syslog:server=unix:/run/httpdog/httpdog.sock`. The binary was a static arm64 build of the working tree, made with the same musl target CI uses, not a binary built by CI.

| Distribution | systemd | nginx | Checks passed |
|---|---|---|---|
| Ubuntu 22.04 | 249 | 1.18.0 | 36 of 36 |
| Ubuntu 24.04 | 255 | 1.24.0 | 36 of 36 |
| Rocky Linux 10 (group `nginx`) | 257 | 1.26.3 | 36 of 36 |
| Debian 11 | 247 | could not be installed | 24 of 24 |

On Debian 11 the nginx packages could not be downloaded that day (the security mirror answered 404), so only the checks that need no nginx ran there: the ones about the unit, the socket's permissions and UDP.

- **The socket**: it is the default `listen`; mode `0660`, owned by the user httpdog runs as, group set by the unit file; httpdog is not a member of that group and does not run as root.
- **Who can send**: `nobody` and an ordinary user are refused and raise nothing; the web server's account and root can send; the `logger` command from the README raises an alert.
- **nginx**: a real request raises an alert in the JSON format and in `combined` (including a quote in the User-Agent); nginx keeps serving while httpdog is stopped.
- **Restarts**: a reload keeps the socket's group; after `systemctl restart` and after `SIGKILL` the new socket is handed to the group again; restart plus an nginx reload loses nothing (30 of 30 requests); a restart alone loses at most one line per worker (3 of 60 with 3 workers).
- **UDP**: works with the unit file unchanged; prints the warning for a loopback address and the stronger one for `0.0.0.0`; `nobody` can send a line that raises an alert, which is what the warning is about.
- **Mistakes**: a reload with a changed `listen` says it is not applied; a socket path without `unix:` ends with exit status 78 and is not restarted; a wrong group in the unit file fails with `chgrp: invalid group`.

GitHub Actions also runs on Ubuntu 24.04 on every push (`.github/workflows/build.yml`): every check in `make ci`, then a musl static build on an amd64 runner and an arm64 runner, and the rule file checks and smoke tests again with the built binaries.

**macOS (Apple Silicon, the development machine)**

- Tested: build, clippy, unit tests, smoke tests on both sample logs.
- `overload` alerts, reload with a broken rule file, the webhook body and signature, and `combined` logs from the Apache 2.4 bundled with macOS were also tested here before the Linux run above.

**Fuzzing**

- The four targets under `fuzz/fuzz_targets` (the two log parsers, the rule engine, rule file loading) were run on macOS with cargo-fuzz, five minutes each (about 7.9 million runs in total), without a crash or a failed assertion.
- Writing the targets turned up one real problem, fixed before the first release: with the JSON format, a newline inside a field value reached the alert text as it was, so a request could add a forged line to the system log. The alert text is now always a single line, and the `rule_engine` target asserts it.
- The fuzz workflow (`.github/workflows/fuzz.yml`) runs the same targets on GitHub Actions (Ubuntu 24.04): a minute each on every push, 15 minutes each once a week.

**Not verified**

- The distributions' own kernels, and SELinux in enforcing mode. With SELinux enforcing, nginx may need a policy that lets it write to the socket; this has not been tried.
- The unix socket input with the binaries CI builds, on amd64, and on Ubuntu 26.04, Debian 12 / 13 and Fedora.
- amd64 on real hardware under systemd (the amd64 machine above was emulated; CI runs the amd64 binary's smoke tests on a real amd64 runner).
- Long-running operation and real production traffic.
- A real external service as the webhook receiver.
