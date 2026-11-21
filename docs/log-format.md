# Log formats and fields

httpdog understands two log formats; pick one with `format` at the top of the rule file. `field` and `group_by` in a rule, and `{...}` in a `message`, all take log field names. Which names are available depends on the format.

| | `format: json` (default) | `format: combined` |
|---|---|---|
| What changes on the web server | nginx gets a `log_format` block | nginx gets one line; Apache uses a pipe |
| Available fields | You decide; the default setup has 10, and httpdog adds `request_path` | A fixed 13, without `host` and `request_time` |
| A mistyped field name | Cannot be detected; the rule just never matches | Rejected at load time |

Fields with the same name mean the same thing in both formats, so most rules work with either.

- [JSON format](#json-format)
- [combined format](#combined-format)
- [Apache](#apache)
- [How logs reach httpdog](#how-logs-reach-httpdog)
- [syslog header](#syslog-header)

## JSON format

`deploy/nginx-snippet.conf` goes inside nginx's `http { }`; the install in the README does that by placing it in `/etc/nginx/conf.d/` (the snippet is shown in the [README](../README.md#option-2-json)). A `server { }` block with an `access_log` line of its own needs the snippet's `access_log syslog:...` line as well: nginx uses the lines of the innermost level that has any, so such a site sends httpdog nothing otherwise. The fields a rule can use are **the keys of the JSON on each log line**: what is available is decided by nginx's `log_format`. The default setup has these 10, and httpdog adds one of its own, `request_path`:

| Field | Type | nginx variable | Content | Example |
|---|---|---|---|---|
| `remote_addr` | String | `$remote_addr` | Client IP. Behind a proxy it is the proxy's IP unless `real_ip` is set up | `203.0.113.7` |
| `host` | String | `$host` | Requested host name, without the port | `example.com` |
| `request_method` | String | `$request_method` | HTTP method, upper case | `POST` |
| `uri` | String | `$uri` | Normalized path, without the query string | `/products` |
| `request_uri` | String | `$request_uri` | The original path the client sent, with the query string, not decoded | `/products?page=2` |
| `request_path` | String | none: httpdog takes it from `request_uri` | The path the client asked for: `request_uri` without the query string, not decoded | `/products` |
| `status` | Number | `$status` | Response status code | `404` |
| `request_time` | Number | `$request_time` | Seconds spent on the request, with millisecond precision | `0.042` |
| `body_bytes_sent` | Number | `$body_bytes_sent` | Bytes in the response body, headers not included | `5120` |
| `http_referer` | String | `$http_referer` | Referer header, an empty string when absent | `https://example.com/` |
| `http_user_agent` | String | `$http_user_agent` | User-Agent header, an empty string when absent | `Mozilla/5.0` |

### Things to watch out for

- **A mistyped name is not an error.** When httpdog loads the rules it does not know which fields the logs will have, so a typo such as `field: stauts` passes validation and simply never matches. After writing a rule, replay a real log with `--stdin` to confirm that it fires.
- **Names must match exactly**, including case (`Status` is not `status`).
- **Only top-level keys work.** Nested paths such as `geo.country` are not supported.
- **Match paths with `request_path`.** There are three fields for the path and they answer different questions:

  | Field | What it is | Use it for |
  |---|---|---|
  | `request_path` | The path the client asked for, without the query string | Matching paths: `/login`, `/.env`, `/admin/` |
  | `request_uri` | The same, with the query string | Parameters, and patterns such as `../` |
  | `uri` | The path nginx ended up serving: decoded and normalized, after internal redirects | What was really served, such as a sensitive file that was read |

- **`request_path` needs `request_uri` in the `log_format`.** httpdog cuts it out of that field, so a `log_format` without `request_uri` has no `request_path`, and a rule on it never matches. A key named `request_path` that your own `log_format` writes is left as it is.
- **`request_path` is what the client sent**, not decoded and not normalized: a request for `/%2eenv` or `/a/../.env` has exactly that as its `request_path`. nginx decodes and resolves those before choosing a file, and that result is `uri`.
- **`uri` is the path after nginx's internal redirects.** On a site that hands requests to a front controller with `try_files ... /index.php` (common with Laravel and WordPress), every line has `uri` set to `/index.php`. A custom error page does the same to failed requests: with `error_page 404 /404.html;`, which the default nginx configuration of Rocky Linux 9 has, a request that ends in a 404 is logged with `uri` set to `/404.html`, so a rule that looks for `/.env` in `uri` does not see a probe that got a 404. `request_path` is not affected by either, which is why the example rules match paths with it.
- **An empty string counts as present.** For a request without a Referer, `http_referer` is `""` and `exists` still holds. To catch empty values write `{ field: http_referer, op: eq, value: "" }`.

### Adding fields

Add a key to `log_format` and rules can use it right away. httpdog needs no change and no restart (nginx needs a reload). For example, to see the backend response time:

```nginx
'"upstream_response_time":"$upstream_response_time",'
```

A variable whose value may not be a number must be **quoted as a string**, as above. `$upstream_response_time` and `$upstream_status` are `-` when the request never reached a backend; without quotes that line is not valid JSON and the whole line is skipped. Numeric comparison still works on a quoted value (`"0.250"` can be compared with `gt`), and `-` counts as no match.

## combined format

This is the default access log format of nginx and Apache:

```
203.0.113.7 - - [11/Nov/2026:13:50:01 +0800] "GET /.env HTTP/1.1" 404 153 "-" "curl/8.5.0"
```

Write `format: combined` at the top of the rule file and httpdog parses it directly; no `log_format` is needed on the web server. For nginx see the [README](../README.md#option-1-the-default-format-combined), for Apache see [below](#apache). `examples/00-combined-starter.yaml` is a starter rule file that works as it is.

The fields are a fixed 13:

| Field | Type | Content | Example |
|---|---|---|---|
| `remote_addr` | String | Client IP | `203.0.113.7` |
| `remote_user` | String | HTTP basic auth user, an empty string when absent | `alice` |
| `time_local` | String | The time on the log line, kept as written | `11/Nov/2026:13:50:01 +0800` |
| `request` | String | The full request line | `GET /products?page=2 HTTP/1.1` |
| `request_method` | String | HTTP method | `GET` |
| `request_uri` | String | Path with the query string | `/products?page=2` |
| `request_path` | String | Path without the query string; the same value as `uri` | `/products` |
| `uri` | String | Path without the query string | `/products` |
| `server_protocol` | String | HTTP version | `HTTP/1.1` |
| `status` | Number | Response status code | `404` |
| `body_bytes_sent` | Number | Bytes in the response; 0 when the log has `-` | `5120` |
| `http_referer` | String | Referer header, an empty string when absent | `https://example.com/` |
| `http_user_agent` | String | User-Agent header, an empty string when absent | `Mozilla/5.0` |

### Differences from the JSON format

- **There is no `host` and no `request_time`**; `combined` simply does not record them. Grouping by site and slow-request rules are therefore not possible. Switch to the JSON format if you need them.
- **Field names are checked at load time.** A name in `field` or `group_by` that is not in the table above (a typo, or `host` / `request_time`) is rejected. Names in `message` are not checked; a wrong one shows as `-`.
- **`uri` and `request_path` are the same value, cut out of the request line**: everything after `?` is removed, without nginx's normalization. Both exist so that a rule written for the JSON format, where they differ, works here unchanged. Encodings such as `%2F` are not decoded and `../` is not resolved. The upside is that `try_files` has no effect on it: you see the path the client sent.
- **`-` always becomes an empty string**, the same as a missing header in the JSON format.
- **When the request line is invalid** (an empty request, garbage, a TLS handshake sent to the HTTP port), `request_method`, `request_uri`, `request_path` and `uri` are empty strings and the original content stays in `request`.
- **Extra fields at the end are ignored.** httpdog does not read fields such as `$request_time` that you append after `combined`; use the JSON format when you need them.
- **Apache's `common` format works too** (it lacks Referer and User-Agent); those two fields are empty strings.

### Quotes and special characters

Escaped characters in the log are restored: nginx writes a quote as `\x22` and Apache as `\"`, and both parse to `"`. The one exception is control characters such as newline and tab. They stay escaped, so nobody can forge alert content by putting a newline in a URL.

The JSON format has no such step, because a JSON string can legitimately hold a newline (nginx writes it as `\n`). The fields keep the real value there, and the protection sits where the alert is written instead: the one-line text of an alert always has control characters in escaped form, in both formats.

This is unrelated to percent-encoding in the URL itself: a non-ASCII path sent by a browser, such as `/café`, appears in the log as `/caf%C3%A9`, and rules must be written in that form.

## Apache

Apache cannot send its access log to syslog directly; it can only write a file or pipe to a program. The rule file needs `format: combined`. There are two ways to connect it:

```bash
# Way 1: follow the log file (-n0 means only new lines)
tail -n0 -F /var/log/apache2/access.log | httpdog --stdin --config rules.yaml
```

```apache
# Way 2: let Apache pipe the log straight into httpdog
CustomLog "|/usr/local/bin/httpdog --stdin --config /etc/httpdog/rules.yaml" combined
```

- **Put the `CustomLog` line inside the `<VirtualHost>` that serves the site.** A virtual host with a `CustomLog` of its own, such as the default site on Debian and Ubuntu, does not write to one defined outside it, so a line added at the top level receives nothing.
- **With way 2, httpdog runs as the user that started Apache, normally root.** Apache starts piped log programs before it drops privileges. Way 1 lets you choose the user.
- **Use way 1 on a busy site.** Way 2 is a direct pipe and `--stdin` never drops logs: when httpdog cannot keep up, Apache blocks while writing the log, which slows requests down.
- **Way 1 has no matching systemd unit yet.** httpdog has no built-in file-reading mode, and the unit files under `deploy/` are for the nginx syslog setup.

## How logs reach httpdog

nginx sends every log line as one syslog message to the address in `listen`, at the top of the rule file. httpdog matches whatever arrives there. It cannot tell a line nginx wrote from a line another program made up, so whoever can send to that address can raise false alerts, or send so much that real lines are dropped. The address should be one that only the web server can reach.

| `listen` | What it is | Who can send logs |
|---|---|---|
| `unix:/run/httpdog/httpdog.sock` (default) | A unix socket: a file systemd creates and passes to httpdog | root, and the one group `httpdog.socket` opens it to |
| `127.0.0.1:5514` | UDP on the loopback address | Every program on the machine |
| `0.0.0.0:5514`, or any address other machines can reach | UDP on the network | Every host that can reach the port, and every program on the machine |

The same address goes into nginx's `syslog:server=`:

```nginx
access_log syslog:server=unix:/run/httpdog/httpdog.sock,tag=nginx,nohostname httpdog;
```

`--stdin` does not use `listen` at all, so none of this applies to the [Apache](#apache) setups above.

### The unix socket

- **systemd creates the socket and holds it; httpdog only reads from it.** `deploy/httpdog.socket` describes the socket: its path, its mode and its group. systemd creates it at boot, before the web server starts, and passes the open socket to httpdog at every start. httpdog never touches the socket file, and cannot remove or replace it.
- **Only root and one group can write to it.** The file belongs to root and to the group in `SocketGroup=`, with mode `0660`. httpdog runs as an unprivileged user that is not in that group, so it gains no access to the web server's files.
- **The group in `httpdog.socket` is `www-data`**, which is right for Debian and Ubuntu. Where nginx's workers run as another group (`nginx` on Fedora and Rocky Linux, or whatever `user` in `nginx.conf` says), change `SocketGroup=`, then `sudo systemctl daemon-reload && sudo systemctl restart httpdog.socket && sudo systemctl reload nginx`. With the wrong group nginx cannot write to the socket: no alerts arrive, and nginx's error log shows `connect() failed (13: Permission denied)`. With a group that does not exist the socket unit fails to start, and `journalctl -u httpdog.socket` shows `Failed to resolve group`.
- **With SELinux enforcing, install `deploy/httpdog-selinux.cil` once**: `sudo semodule -i httpdog-selinux.cil`. This is the default state of Fedora, RHEL, Rocky Linux and AlmaLinux. Without it SELinux refuses to let nginx send to the socket: no alerts arrive and nginx's error log shows `connect() failed (13: Permission denied)`, the same message a wrong group gives; `sudo ausearch -m AVC -ts recent` then lists a `sendto` denial for nginx. The file holds a single rule, which lets the web server's domain (`httpd_t`) send a datagram to the socket of a service without a domain of its own (`unconfined_service_t`, which is what httpdog runs as). Keep the default socket path there: SELinux labels `/run/httpdog` as a directory the web server may write to, and does not do that for a path chosen elsewhere.
- **A program running as the web server's group can still send logs.** That includes a web application that runs under the same account as nginx's workers, such as PHP-FPM running as `www-data`. The socket keeps out everything else on the machine; it does not make the logs trustworthy against the web server's own account.
- **A restart of httpdog loses no lines.** The socket stays the same one, so nginx stays connected and needs no reload. Lines sent while httpdog is away wait in the socket and are read when it is back: after `systemctl restart`, after a crash, after the watchdog or the memory ceiling ends it. The socket holds a few hundred lines, and there are two limits: 512 in all (the kernel's `net.unix.max_dgram_qlen` under systemd), and what each nginx worker may have waiting at a time, about 180 lines, or about 100 when lines are close to 1000 bytes long. Lines beyond that are dropped, and nginx does not report them. A restart takes well under a second, so this matters for a busy site or a long outage, not for an ordinary restart. What httpdog had already taken off the socket when it was killed is lost with it.
- **To stop httpdog, stop the socket with it**: `sudo systemctl stop httpdog.socket httpdog`. Stopping httpdog alone leaves the socket with systemd, and the next log line starts httpdog again. While both are stopped the socket does not exist: nginx keeps serving requests as usual and writes `connect() failed (2: No such file or directory) while logging to syslog` to its error log for the lines it could not send.
- **Restarting `httpdog.socket` makes a new socket**, and restarts httpdog with it. Each nginx worker still holds the old one: it finds out on its next log line, which is lost, and connects to the new socket for the line after that. Reload nginx after it (`sudo systemctl reload nginx`) and nothing is lost. This is needed only after changing `httpdog.socket`.
- **A rule file error at startup also ends with the socket gone.** httpdog exits with status 78 and systemd leaves it down. The first log line that arrives after that makes systemd start it again, a few more times (until its start limit, 5 starts in 10 seconds by default), and then systemd gives up on `httpdog.socket` as well and removes the socket. After fixing the rule file, `sudo systemctl start httpdog && sudo systemctl reload nginx`; a start within those same 10 seconds is refused (`Start request repeated too quickly` in the journal) unless `sudo systemctl reset-failed httpdog` comes first. Check a rule file with `--check` before a restart, as before a reload, and this never happens.
- **To use another path**, change `ListenDatagram=` in `httpdog.socket` and `listen` in the rule file to the same path, and nginx's `server=` too. Then `sudo systemctl daemon-reload && sudo systemctl restart httpdog.socket && sudo systemctl reload nginx`. httpdog compares what systemd passed with `listen` and refuses to start when they differ (exit status 78), because it would otherwise run and receive nothing. A reload is checked the same way: a rule file whose `listen` is not the socket systemd holds is refused and the old rules stay, so the mistake shows when the file is changed and not at the next reboot.
- **Started without `httpdog.socket`**, by hand or by another supervisor, httpdog creates the socket itself, with mode `0660` and the group it runs as; when the directory has the setgid bit it gets the directory's group instead. Check that the web server's workers are in that group and that nothing else is. The directory has to exist. A socket file left by an earlier run is replaced; if another httpdog is still listening on it, the new one refuses to start instead of taking it over, and a file that is not a socket is never removed. Started this way, a restart does create a new socket, and nginx has to be reloaded after it.

### UDP

UDP is still there for the cases a unix socket cannot cover, such as a web server on another machine. The address goes in three places: `listen: 127.0.0.1:5514` in the rule file, `ListenDatagram=127.0.0.1:5514` in `httpdog.socket` (in place of the socket path; systemd holds this socket too, so a restart loses no lines here either), and nginx's `syslog:server=`. Then `sudo systemctl daemon-reload && sudo systemctl restart httpdog.socket`. httpdog refuses to start when `listen` and `ListenDatagram=` differ.

Because UDP cannot limit who sends, httpdog prints a warning at every start:

```
warning: listening on UDP 127.0.0.1:5514: any program on this machine can send logs here and make httpdog raise false alerts. ...
```

When the address is reachable from other machines, limit the port to the web server with a firewall and keep it on a private network. The source address of a UDP packet can be forged, so a firewall rule narrows who can send but does not prove who did.

## syslog header

Every line nginx sends over syslog starts with a header (for example `<190>Nov 11 12:00:00 nginx: `). httpdog reads lines with or without it:

- `format: json`: parsing starts at the first `{`.
- `format: combined`: a line starting with `<` has its header removed first (the RFC 3164 format nginx sends).
