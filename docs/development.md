# Development

## Common commands

Rust 1.85 or later is required (edition 2024). CI and releases use the latest stable at the time; the most recent local build and test run used 1.100.0.

```bash
cargo build --release
cargo test                    # Unit tests (rule engine, duration parsing, log format parsing, webhook body and signature)
cargo clippy --all-targets    # Must be warning-free
cargo fmt
```

All of these are wrapped in the `Makefile`. `make ci` runs the format check, clippy, the unit tests, `--check` on every example rule file and the smoke tests on both sample logs, in that order; run it before committing. `make` with no arguments lists every target.

## Fuzzing

Log content comes from the requests a site receives, so the code that reads it is fuzzed with [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) (libFuzzer). The targets live in `fuzz/fuzz_targets`:

| Target | What it checks |
|---|---|
| `parse_json` | The `json` parser never panics on arbitrary bytes, and what it returns is a JSON object |
| `parse_combined` | The `combined` parser never panics, a parsed line has all 13 fields, and unescaping never produces a control character |
| `rule_engine` | The whole path of a log line (parse, match with every operator, thresholds, cooldowns, message template) never panics, and the text of an alert is always a single line |
| `rule_file` | Loading an arbitrary rule file either succeeds or returns an error, never a panic |

```bash
rustup toolchain install nightly && cargo install cargo-fuzz --locked   # once
make fuzz                 # every target at once, 60 seconds each
make fuzz FUZZTIME=300    # longer
```

An input that fails is saved under `fuzz/artifacts/<target>/`. Replay it with `cargo +nightly fuzz run <target> fuzz/artifacts/<target>/<file>`, fix the bug, and add a unit test for it. `fuzz/` is a separate crate with its own lock file, so its dependencies never reach the httpdog binary.

## How the program works

```
nginx ──syslog (unix socket or UDP)──▶ input thread ──▶ main loop (rule engine) ──▶ notifier thread ──▶ webhook / stderr
```

Receiving logs, matching and sending notifications are independent: slow notifications never block matching, and when matching cannot keep up it is the incoming logs that are dropped, so memory cannot grow without bound.

## Files

| File | Content |
|---|---|
| `src/lib.rs` | Exposes the modules below as a library, so the binary and the fuzz targets run the same code |
| `src/config.rs` | Data structures of the rule file; only reads the file |
| `src/engine.rs` | Rule engine: rule file validation, matching, threshold counting, cooldowns |
| `src/format.rs` | Parses a log line into fields: `json` and `combined` |
| `src/notify.rs` | Notifier thread, webhook body, signature and retries |
| `src/main.rs` | Arguments, signals, socket / stdin input, main loop, systemd notification |
| `deploy/` | The two systemd units (`httpdog.socket` holds the socket logs arrive on, `httpdog.service` runs the program), nginx config snippet, and the SELinux policy for systems where it is enforcing |
| `tests/` | Starts the built binary the way systemd does, with the socket passed in, and checks that a restart keeps it (Linux only) |
| `examples/` | The `combined` starter rules and 5 scenario templates |
| `fuzz/` | Fuzz targets and the script that runs them |
| `sample.log`, `sample.combined.log` | Sample logs for the smoke tests (one JSON, one `combined`) |
| `Makefile` | Command shortcuts for development (`build`, `test`, `lint`, `smoke`, `ci` and so on) |
| `CHANGELOG.md` | Changes in each version; also the source of the release notes |
| `.github/` | The GitHub Actions workflows, the Dependabot config and the script that extracts the release notes |
| `SECURITY.md`, `CONTRIBUTING.md` | How to report a vulnerability, and the rules a contribution must follow |
| `LICENSE` | License (MIT) |

## CI and releases

On every push to `main`, GitHub Actions runs `make ci` and builds static binaries for amd64 and arm64, kept as artifacts of that run (`httpdog-linux-amd64`, `httpdog-linux-arm64`). Artifacts need a GitHub login to download, and they expire.

Three more workflows run next to it:

| Workflow | What it does |
|---|---|
| `fuzz.yml` | Runs every fuzz target, a minute each on a push or pull request and 15 minutes each once a week |
| `codeql.yml` | CodeQL static analysis of the Rust code and of the workflows |
| `scorecard.yml` | [OpenSSF Scorecard](https://scorecard.dev/viewer/?uri=github.com/guanting112/httpdog): scores the repository's supply chain practices and publishes the result |

`codeql.yml` and `scorecard.yml` skip themselves while the repository is private, because code scanning and published Scorecard results are only available to public repositories. Dependabot (`.github/dependabot.yml`) opens weekly pull requests for new versions of crates and actions.

When a tag starting with `v` is pushed (e.g. `v0.1.0`), the same workflow, once it passes, publishes the two binaries and `SHA256SUMS` as a Release, with notes taken from the matching section of `CHANGELOG.md`. The release job also signs a build provenance attestation for the binaries and attaches it as `httpdog.sigstore.json`, and again under the name `httpdog.intoto.jsonl`, which is the one OpenSSF Scorecard recognises as provenance; this step needs a public repository. The tag must match the version in `Cargo.toml` and `CHANGELOG.md` must have a section for that version, or the workflow fails before building.

Release steps:

1. Change the version in `Cargo.toml`, then run `cargo build` once so the version in `Cargo.lock` follows. CI builds with `--locked` and fails right away if `Cargo.lock` is stale.
2. `CHANGELOG.md`: move what is under `[Unreleased]` into a new `## [x.y.z] - date` section, and add the link definition at the end of the file.
3. Run `.github/release-notes.sh vX.Y.Z` locally and confirm that it prints the notes.
4. `make ci`, commit, push to `main`, and wait for CI to pass.
5. `git tag vX.Y.Z && git push origin vX.Y.Z`.

Never push a tag with the same name as a published version again; if something is wrong, release the next version.
