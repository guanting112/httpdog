# Log formats and fields

httpdog understands two log formats; pick one with `format` at the top of the rule file. `field` and `group_by` in a rule, and `{...}` in a `message`, all take log field names. Which names are available depends on the format.

| | `format: json` (default) | `format: combined` |
|---|---|---|
| What changes on the web server | nginx gets a `log_format` block | nginx gets one line; Apache uses a pipe |
| Available fields | You decide; the default setup has 10 | A fixed 12, without `host` and `request_time` |
| A mistyped field name | Cannot be detected; the rule just never matches | Rejected at load time |

Fields with the same name mean the same thing in both formats, so most rules work with either.

- [JSON format](#json-format)
- [combined format](#combined-format)
- [Apache](#apache)
- [syslog header](#syslog-header)

## JSON format

Put the contents of `deploy/nginx-snippet.conf` inside nginx's `http { }` (shown in the [README](../README.md#option-2-json)). The fields a rule can use are **the keys of the JSON on each log line**: httpdog has no built-in fields, and what is available is decided entirely by nginx's `log_format`. The default setup has these 10:

| Field | Type | nginx variable | Content | Example |
|---|---|---|---|---|
| `remote_addr` | String | `$remote_addr` | Client IP. Behind a proxy it is the proxy's IP unless `real_ip` is set up | `203.0.113.7` |
| `host` | String | `$host` | Requested host name, without the port | `example.com` |
| `request_method` | String | `$request_method` | HTTP method, upper case | `POST` |
| `uri` | String | `$uri` | Normalized path, without the query string | `/products` |
| `request_uri` | String | `$request_uri` | The original path the client sent, with the query string, not decoded | `/products?page=2` |
| `status` | Number | `$status` | Response status code | `404` |
| `request_time` | Number | `$request_time` | Seconds spent on the request, with millisecond precision | `0.042` |
| `body_bytes_sent` | Number | `$body_bytes_sent` | Bytes in the response body, headers not included | `5120` |
| `http_referer` | String | `$http_referer` | Referer header, an empty string when absent | `https://example.com/` |
| `http_user_agent` | String | `$http_user_agent` | User-Agent header, an empty string when absent | `Mozilla/5.0` |

### Things to watch out for

- **A mistyped name is not an error.** When httpdog loads the rules it does not know which fields the logs will have, so a typo such as `field: stauts` passes validation and simply never matches. After writing a rule, replay a real log with `--stdin` to confirm that it fires.
- **Names must match exactly**, including case (`Status` is not `status`).
- **Only top-level keys work.** Nested paths such as `geo.country` are not supported.
- **`uri` versus `request_uri`**: `uri` is the normalized path without the query string; `request_uri` is exactly what the client sent, with the query string. Use `request_uri` to match parameters.
- **`uri` is the path after nginx's internal redirects.** On a site that hands requests to a front controller with `try_files ... /index.php` (common with Laravel and WordPress), every line has `uri` set to `/index.php`. On such a site match paths with `request_uri`; it carries the query string, so use `prefix` or `regex` rather than `eq` / `suffix`.
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

The fields are a fixed 12:

| Field | Type | Content | Example |
|---|---|---|---|
| `remote_addr` | String | Client IP | `203.0.113.7` |
| `remote_user` | String | HTTP basic auth user, an empty string when absent | `alice` |
| `time_local` | String | The time on the log line, kept as written | `11/Nov/2026:13:50:01 +0800` |
| `request` | String | The full request line | `GET /products?page=2 HTTP/1.1` |
| `request_method` | String | HTTP method | `GET` |
| `request_uri` | String | Path with the query string | `/products?page=2` |
| `uri` | String | Path without the query string | `/products` |
| `server_protocol` | String | HTTP version | `HTTP/1.1` |
| `status` | Number | Response status code | `404` |
| `body_bytes_sent` | Number | Bytes in the response; 0 when the log has `-` | `5120` |
| `http_referer` | String | Referer header, an empty string when absent | `https://example.com/` |
| `http_user_agent` | String | User-Agent header, an empty string when absent | `Mozilla/5.0` |

### Differences from the JSON format

- **There is no `host` and no `request_time`**; `combined` simply does not record them. Grouping by site and slow-request rules are therefore not possible. Switch to the JSON format if you need them.
- **Field names are checked at load time.** A name in `field` or `group_by` that is not in the table above (a typo, or `host` / `request_time`) is rejected. Names in `message` are not checked; a wrong one shows as `-`.
- **`uri` is cut out of the request line**: everything after `?` is removed, without nginx's normalization. Encodings such as `%2F` are not decoded and `../` is not resolved. The upside is that `try_files` has no effect on it: you see the path the client sent.
- **`-` always becomes an empty string**, the same as a missing header in the JSON format.
- **When the request line is invalid** (an empty request, garbage, a TLS handshake sent to the HTTP port), `request_method`, `request_uri` and `uri` are empty strings and the original content stays in `request`.
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

- **Use way 1 on a busy site.** Way 2 is a direct pipe and `--stdin` never drops logs: when httpdog cannot keep up, Apache blocks while writing the log, which slows requests down.
- **Way 1 has no matching systemd unit yet.** httpdog has no built-in file-reading mode, and `deploy/httpdog.service` is for the nginx syslog setup.

## syslog header

Every line nginx sends over syslog starts with a header (for example `<190>Nov 11 12:00:00 nginx: `). httpdog reads lines with or without it:

- `format: json`: parsing starts at the first `{`.
- `format: combined`: a line starting with `<` has its header removed first (the RFC 3164 format nginx sends).
