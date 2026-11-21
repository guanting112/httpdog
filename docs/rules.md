# Rule file

The full reference for the rule file. If this is your first time, start with ["Write rules" in the README](../README.md#4-write-rules).

- [What the file looks like](#what-the-file-looks-like)
- [Top-level keys](#top-level-keys)
- [notifiers: where notifications go](#notifiers-where-notifications-go)
- [rules: the rules](#rules-the-rules)
- [when: conditions](#when-conditions)
- [overload: alerts when falling behind](#overload-alerts-when-falling-behind)
- [What happens on a mistake](#what-happens-on-a-mistake)
- [Using the rule templates](#using-the-rule-templates)

## What the file looks like

A complete example is in `rules.example.yaml`. A file ending in `.json` is parsed as JSON, anything else as YAML; the structure is identical.

```yaml
listen: unix:/run/httpdog/httpdog.sock
instance: web-01

notifiers:
  console:
    type: stderr
  ops:
    type: webhook
    url_env: HTTPDOG_WEBHOOK_URL
    secret_env: HTTPDOG_WEBHOOK_SECRET

rules:
  - id: login-bruteforce
    when:
      all:
        - { field: request_path, op: prefix, value: /login }
        - { field: request_method, op: eq, value: POST }
        - { field: status, op: in, value: [401, 403] }
    group_by: remote_addr
    threshold: { count: 20, window: 60s }
    cooldown: 10m
    notify: [console, ops]
    message: "{remote_addr} repeated login failures"
```

## Top-level keys

| Key | Required | Description |
|---|---|---|
| `rules` | Yes | The list of rules, see [rules](#rules-the-rules) |
| `notifiers` | No | Notification channels. You choose the names; rules refer to them in `notify` |
| `format` | No | Log format: `json` (default) or `combined`, see [log-format.md](log-format.md). Takes effect on reload |
| `instance` | No | Name of this httpdog instance, e.g. `web-01`. The webhook body carries it so the receiver can tell which one fired. **Default `unknown`**. At most 64 characters; must not be empty or contain newlines or other control characters. Takes effect on reload |
| `listen` | No | Where logs arrive: `unix:/path` for a unix socket, or `host:port` for UDP. **Default `unix:/run/httpdog/httpdog.sock`**. It must match nginx's `syslog:server=`. A unix socket limits who can send logs; with UDP anything that can reach the address can, so httpdog prints a warning at every start. See [How logs reach httpdog](log-format.md#how-logs-reach-httpdog). Under systemd the socket comes from `deploy/httpdog.socket`, and `listen` has to name the same address as `ListenDatagram=` there; httpdog does not start when they differ, and a reload refuses a rule file that changes it (`reload failed, keeping the old rules`). To change it, change `ListenDatagram=` too and restart `httpdog.socket`. Without `httpdog.socket`, changing it needs a restart; a reload takes the other changes and says that this one is not applied |
| `overload` | No | Who to notify when logs are dropped because httpdog cannot keep up, see [overload](#overload-alerts-when-falling-behind). When omitted, drops are only logged |

## notifiers: where notifications go

Each notifier gets a name (`console` and `ops` above). A rule picks where to send with `notify: [name, ...]`.

| type | Parameters | Description |
|---|---|---|
| `stderr` | None | Writes to the system log (journald). Start with this while testing new rules |
| `webhook` | `url_env` or `url` (one of them), `secret_env` or `secret` (**required**, one of them) | POSTs the alert as JSON to your own receiver |

Webhook parameters:

- `url_env`: the environment variable to read the URL from. Use this in production: the URL lives in `/etc/httpdog/secrets.env`, not in the rule file.
- `url`: the URL written directly in the rule file. For testing only.
- `secret_env`: the environment variable to read the signing secret from. **httpdog will not start without a secret.**
- `secret`: the secret written directly in the rule file. For testing only.

For what is sent, how to verify the signature and how retries work, see [webhook.md](webhook.md).

## rules: the rules

| Key | Required | Description |
|---|---|---|
| `id` | Yes | Rule name. Must be unique; it appears in notifications |
| `when` | Yes | The condition, see [when](#when-conditions) |
| `notify` | Yes | List of notifier names to notify, at least one |
| `threshold` | No | `{ count, window }`: notify only once `count` hits accumulate within `window`, then start counting again. When omitted, a single hit notifies |
| `group_by` | No | Grouping field. Thresholds and cooldowns are tracked separately per value of this field, e.g. `remote_addr` counts each IP on its own. When omitted, the whole rule shares one group. Each value that is being counted takes a little memory until its window and cooldown have passed, so prefer a field with a limited number of values; see [limits.md](limits.md#known-limitations) |
| `cooldown` | No | Notify at most once per group within this period. **Default `1m`**; `0s` means no cooldown |
| `message` | No | The notification text. `{field}` is replaced with that log line's value, or `-` when the field is missing. When omitted, the first 300 characters of the raw log line are attached |

Durations are written as `30s`, `10m`, `2h`; a bare number means seconds.

The one-line notification text looks like this. The last two parts appear only when `threshold` / `group_by` are set:

```
[httpdog] <id> | <message> | <hits>x in <window>s | <group_by>=<value>
```

This text is always a single line. If a value from the log contains a newline or another control character, it is written in escaped form (`\n`), so a request cannot add a line of its own to the system log.

## when: conditions

The basic condition is `{ field, op, value }`: take the value of `field` in the log line and compare it with `value` using `op`. For what `field` can be, see [log-format.md](log-format.md).

Combine conditions with `all` (every one holds), `any` (at least one holds) and `not` (negation). They nest to any depth:

```yaml
when:
  all:
    - { field: request_time, op: gt, value: 5 }
    - not: { field: request_path, op: prefix, value: /export/ }
    - any:
        - { field: host, op: eq, value: example.com }
        - { field: host, op: suffix, value: .example.com }
```

This reads: the request took more than 5 seconds, and the path does not start with `/export/`, and the site is `example.com` or one of its subdomains.

### Operators (op)

| op | What value must be | Description |
|---|---|---|
| `eq` / `ne` | String, number or boolean | Equal, not equal |
| `in` | List (must not be empty) | Equal to any item in the list |
| `prefix` / `suffix` / `contains` | String | Starts with, ends with, contains |
| `regex` | String | Regular expression (Rust `regex` syntax; no lookaround or backreferences) |
| `gt` / `gte` / `lt` / `lte` | Number | Greater than, greater or equal, less than, less or equal |
| `exists` | Not needed | The field is present |

### Matching details

- **A field missing from the log line never matches** (the same goes for a `null` value), not even with `ne`. To express "this field is absent", write `not: { field: x, op: exists }`.
- **Numbers match loosely**: when `value` is a number the comparison is numeric, so both `"status":"404"` and `"status":404` in the log match. When `value` is a string the comparison is by string.
- **regex matches anywhere**: a match on any part counts. Add `^` and `$` yourself to match the whole string. In YAML, single quotes are recommended so backslashes need no extra escaping.
- **Always case-sensitive**: `/Admin` is not `/admin`.

### Case-insensitive matching

Only `regex` can ignore case: put `(?i)` at the start of the pattern, or wrap just one part with `(?i:...)`. The other ops have no such option; rewrite them as a regex:

| Original | Case-insensitive version |
|---|---|
| `op: eq, value: /admin` | `op: regex, value: '(?i)^/admin$'` |
| `op: prefix, value: /admin` | `op: regex, value: '(?i)^/admin'` |
| `op: suffix, value: .php` | `op: regex, value: '(?i)\.php$'` |
| `op: contains, value: bot` | `op: regex, value: '(?i)bot'` |

When rewriting, remember to put a backslash before characters that are special in a regex, such as `.`, `?` and `+`.

The case of `request_path`, `uri`, `request_uri` and `http_user_agent` is up to the client, and attackers often mix case on purpose, so take extra care when matching these fields.

## overload: alerts when falling behind

When traffic exceeds what httpdog can process, it drops the logs it cannot keep up with (see [known limitations](limits.md)). A drop is always written to the system log; with this section it is also sent through a notifier:

```yaml
overload:
  notify: [ops]
  cooldown: 5m
```

| Key | Required | Description |
|---|---|---|
| `notify` | Yes | List of notifier names to notify, at least one |
| `cooldown` | No | Minimum interval between two alerts, **default `1m`**. Drops in between accumulate and are reported together when the cooldown ends |

The notification text is `[httpdog] overload | falling behind, dropped <n> log lines`, where the number is what was dropped since the previous overload alert. The first alert goes out within about a second of the first drop; after traffic subsides, the count accumulated during the cooldown is reported in one more alert when the cooldown ends.

This alert only covers logs that httpdog itself dropped:

- Packets the operating system dropped before they reached the program are invisible to httpdog; check them with `netstat -su`.
- Alerts dropped because the notification queue is full (usually the webhook is down) are still only logged, because another alert could not be delivered either.
- `--stdin` mode never drops logs, so it never fires.

## What happens on a mistake

The rule file is checked completely when it is loaded. A mistake is reported with its location right away instead of surfacing at run time:

- a mistyped key name, or an extra unknown key
- an invalid regex, a mistyped op, a `value` of the wrong type
- `notify` refers to a notifier that does not exist
- a webhook without a signing secret, or one whose environment variable is not set

At startup, httpdog exits with status 78 and systemd does not restart it. On a reload, it keeps running with the old rules and writes the reason to the log.

The one thing that cannot be checked is a field name under `format: json`: a typo such as `field: stauts` passes validation and simply never matches. See [log-format.md](log-format.md#things-to-watch-out-for) for why. So after writing a rule, replay a real log with `--stdin` to confirm that it fires.

## Using the rule templates

The 6 templates under `examples/` are listed in the [README](../README.md#ready-made-rule-templates). `00` is a starter for the `combined` format; `01` to `05` are organized by scenario and assume the JSON format. Before using them:

- **Thresholds and paths are placeholders.** Set thresholds against your site's normal traffic and replace the paths (`/login`, `/checkout`, `/admin` and so on) with the real ones. Replay a recent access log with `--stdin` first and see how often they fire before going live.
- **To combine several files**, concatenate their `rules` lists into one rule file and keep a single `notifiers` section. Rule `id`s must be unique.
- **`01` to `05` all refer to `HTTPDOG_WEBHOOK_URL` and `HTTPDOG_WEBHOOK_SECRET`**; set both environment variables before checking the rule file. `00` only writes to the system log and needs neither.
- **`01` to `05` only use fields from `deploy/nginx-snippet.conf`**, so they work once nginx is set up that way.
- **`01` to `05` also work with `combined`**: add a `format: combined` line at the top of the file. `01`, `02` and `04` work as they are. `host-5xx`, `slow-request` and `slow-burst` in `03` and `payment-callback-slow` in `05` use fields `combined` does not have and fail at load time, so delete those rules first. `{host}` and `{request_time}` in messages show as `-`.
- **The templates match paths with `request_path`**, the path the client asked for, so they work on a site with a front controller (`try_files ... /index.php`) or custom error pages. The one exception is `sensitive-file-exposed`, which matches `uri`, the file nginx actually served; see [log-format.md](log-format.md#things-to-watch-out-for) for the difference.
