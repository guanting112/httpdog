# Security policy

`httpdog` reads access logs, and their content comes from the requests your site receives: paths, query strings, User-Agent and Referer headers are all chosen by whoever sends the request. A flaw in how httpdog handles that content could let a request hide an attack, fake an alert or stop the monitoring. Please report vulnerabilities privately.

## Supported versions

Only the latest release receives security fixes. A fix ships as a new release; older releases are not patched.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting:

https://github.com/guanting112/httpdog/security/advisories/new

Do not open a public issue, pull request or discussion for a vulnerability.

Please include:

- the output of `httpdog --version`
- the operating system and architecture
- the log format (`json` or `combined`), the rule file, and the log line or request that reproduces the problem, with anything you do not want to publish removed
- what happened and what you expected instead

## What to expect

- You will get an acknowledgement within 7 days.
- A confirmed vulnerability is fixed in a new release, normally within 30 days. If it needs longer, you will be told why.
- The fix is announced in a GitHub security advisory, crediting you unless you prefer otherwise. Please keep the details private until that advisory is published.

## Scope

httpdog treats log content as hostile, so these are vulnerabilities:

- log content that makes httpdog crash, hang or use memory without bound
- log content that forges or alters an alert, such as adding a line to the system log or getting terminal control sequences into the alert text, or that keeps other alerts from firing
- a webhook request that passes signature verification, as described in [docs/webhook.md](docs/webhook.md), without knowledge of the secret
- a webhook URL or signing secret that appears in a log line or an error message
- a release binary that does not match this repository's source and workflow

These are ordinary bugs; please open a public issue for them:

- a rule that does not match what it should, or an alert that should not have fired
- anything that requires changing the rule file or the environment httpdog runs in. Whoever controls those controls httpdog
- log lines sent by a program that is allowed to send to the address httpdog listens on. httpdog trusts what arrives there. By default that is a unix socket open to root and the web server's group only; with a UDP `listen` it is anything that can reach the address, which httpdog warns about at every start (see [docs/log-format.md](docs/log-format.md#how-logs-reach-httpdog)). A program that gets a line in without that access is a vulnerability
- logs dropped when traffic exceeds what httpdog can process. That is a documented limit, and the `overload` alert reports it (see [docs/limits.md](docs/limits.md))
