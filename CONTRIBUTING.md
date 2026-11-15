# Contributing

Bug reports and pull requests are welcome. Please write them in English.

## Reporting a bug

Open an issue at https://github.com/guanting112/httpdog/issues and include:

- the output of `httpdog --version`
- the operating system and architecture
- the log format (`json` or `combined`) and the rule that misbehaves
- a log line that reproduces the problem, and what you expected instead

Log lines hold IP addresses, paths and query strings. Remove what you do not want to publish before pasting them.

A vulnerability in httpdog itself is not an ordinary bug. Report it privately as described in [SECURITY.md](SECURITY.md).

## Proposing a change

For a new operator, log format, notifier or rule file key, open an issue first, so we can agree on the scope before you write code. Small fixes can go straight to a pull request.

Scope: httpdog matches access log lines against rules and notifies. It does not store logs, has no query interface and does not block traffic.

Pull requests go against `main`. CI runs the format check, clippy, the unit tests, the example rule file checks, the smoke tests and a short fuzz run, plus CodeQL, and a pull request is merged only when they pass. Dependabot's pull requests follow the same rule.

## Design rules

These are deliberate decisions. A change that breaks one will not be merged.

- **Log content is hostile.** It comes from the requests a site receives. A parser returns `None` for a line it cannot read and never panics, unescaping never turns an escape sequence into a control character, and the one-line text of an alert never contains a control character.
- **Webhooks are always signed.** A webhook without a secret does not load, and there is no option to turn signing off. The signature covers the exact bytes sent.
- **A webhook has one body.** Adding a field is fine. Renaming or removing one, or changing its type, breaks other people's receivers and needs a new `version`.
- **Configuration errors are found at load time.** Every check sits on the `Engine::from_config` path. Processing a log line cannot fail or panic.
- **A failed reload keeps the old rules.** Monitoring must not stop because of a mistake in the rule file.
- **The main loop never blocks.** Anything that may be slow (network, DNS, disk) runs on the notifier thread or a thread of its own. The systemd watchdog is fed from the main loop only, or it could no longer tell that the loop hangs.
- **Every queue is bounded.** A full queue drops and logs. The one exception is `--stdin`, which waits, because replaying old logs must not lose lines.
- **Secrets stay out of the logs.** The webhook URL and the signing secret never appear in a log line or an error message; an error names the environment variable, not its value.
- **Exit status 78 means a rule file error** and nothing else. The systemd unit relies on it to stop restarting.
- **systemd does the rest.** No daemonizing, no PID file, no timestamps of its own; logs go to stderr.
- **Small and synchronous.** One static binary, no async runtime, few dependencies. A new dependency needs a reason.

## Tests

Every change in behavior comes with tests:

- New or changed logic gets unit tests. A bug fix gets a test that fails without the fix.
- Tests of thresholds and cooldowns pass the time in as an argument and never sleep.
- A new check on the rule file gets a case in `bad_configs_are_rejected_at_load`.
- A new parser of log content gets a fuzz target under `fuzz/fuzz_targets`.

```sh
make ci      # format check, clippy, unit tests, example rule file checks, smoke tests
make fuzz    # every fuzz target at once, 60 seconds each (FUZZTIME=300 for longer)
```

`make fuzz` needs the nightly toolchain and cargo-fuzz: `rustup toolchain install nightly && cargo install cargo-fuzz --locked`.

## Style

- Format with `cargo fmt` and keep `cargo clippy --all-targets` free of warnings.
- Code comments, program output and documentation are in English. `README.zh-TW.md` mirrors `README.md`, so change both.
- Commit messages start with a short summary in the imperative ("Add a CIDR operator"). The body says why the change is needed and what it does differently.
- Add a line to the `Unreleased` section of [CHANGELOG.md](CHANGELOG.md) for anything a user would notice.
- A change to the rule file, the log fields or the webhook body also updates the matching file under `docs/`.

## License

By contributing, you agree that your contribution is released under the [MIT License](LICENSE).
