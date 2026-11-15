# webhook

A webhook is how httpdog hands alerts to your own program: each alert is sent as JSON in an HTTP POST to the URL you choose. Every part of the alert has its own field, so the receiver can route by rule, write to a database, or format it for display.

- [Setup](#setup)
- [What is sent](#what-is-sent)
- [Signature: confirming the request really came from httpdog](#signature-confirming-the-request-really-came-from-httpdog)
- [Receiver example](#receiver-example)
- [Testing your receiver](#testing-your-receiver)
- [When delivery fails](#when-delivery-fails)

## Setup

```yaml
instance: web-01                          # Name of this instance; appears in the instance field of the body

notifiers:
  ops:
    type: webhook
    url_env: HTTPDOG_WEBHOOK_URL          # The URL is kept in an environment variable
    secret_env: HTTPDOG_WEBHOOK_SECRET    # Signing secret, required, also kept in an environment variable
```

Both the URL and the secret live in `/etc/httpdog/secrets.env`, not in the rule file:

```
HTTPDOG_WEBHOOK_URL=https://<your receiver>/...
HTTPDOG_WEBHOOK_SECRET=<a value from openssl rand -hex 32>
```

- You choose the environment variable names; they only have to match between the rule file and `secrets.env`. With several webhooks, give each its own names.
- Use a long random string as the secret, for example one from `openssl rand -hex 32`.
- For testing you can write the values in the rule file with `url:` and `secret:`. This is not recommended in production.
- After changing `secrets.env`, restart. A reload does not read environment variables again.

## What is sent

A webhook has this one body. There is no setting to switch it.

```json
{
  "source": "httpdog",
  "instance": "web-01",
  "version": 1,
  "kind": "rule",
  "rule": "login-bruteforce",
  "timestamp": 1794416274,
  "text": "[httpdog] login-bruteforce | 198.51.100.9 repeated login failures | 3x in 60s | remote_addr=198.51.100.9",
  "message": "198.51.100.9 repeated login failures",
  "group_by": "remote_addr",
  "group": "198.51.100.9",
  "hits": 3,
  "window_secs": 60,
  "dropped": null,
  "event": { "remote_addr": "198.51.100.9", "uri": "/login", "status": 401 }
}
```

| Key | Type | Description |
|---|---|---|
| `source` | String | Always `httpdog` |
| `instance` | String | Which httpdog sent it: `instance` at the top of the rule file, or `unknown` when not set |
| `version` | Number | Version of this format, currently `1`. It goes up only for an incompatible change to the fields, not when a field is added |
| `kind` | String | `rule` (a rule matched) or `overload` (logs were dropped because httpdog could not keep up) |
| `rule` | String | The rule's `id`. Always `overload` for overload alerts |
| `timestamp` | Number | When the alert fired, in Unix seconds. This is when httpdog received the log, not the time written in the log |
| `text` | String | The whole alert as one line, identical to the line in the system log. Use this to show the alert to people as it is. It never contains a control character: a newline or similar inside a value is written in escaped form (`\n`) |
| `message` | String | The message alone: the rule's `message` after substitution, or the first 300 characters of the raw log line when the rule has no `message` |
| `group_by` | String or null | The rule's `group_by` field name |
| `group` | String or null | This group's value, such as an IP |
| `hits` | Number or null | Hits accumulated within the threshold window. null when the rule has no `threshold` |
| `window_secs` | Number or null | Length of the threshold window in seconds |
| `dropped` | Number or null | Set only for `overload` alerts: how many lines this report covers |
| `event` | Object or null | The log line that triggered the alert; see [log-format.md](log-format.md) for the fields. For a threshold rule it is the last line, the one that reached the count; null for `overload` alerts |

- **`message`, `group` and `event` hold the values as they are in the log**, which a client can choose. Unlike `text`, they may contain newlines or other control characters; JSON encoding keeps them safe in transit, but escape them before writing them to a log or a terminal.
- **Every key is always present**, with null where it does not apply, so the receiver never has to handle a missing key. The order of keys is not guaranteed.
- **When several httpdog instances send to one receiver, tell them apart by `instance`.** It is part of the request body and covered by the signature, so tampering with it in transit makes verification fail.

## Signature: confirming the request really came from httpdog

With the URL alone, anyone who knows the webhook URL can send it fake alerts. So httpdog requires that **every webhook has a secret, and it will not start without one** (exit status 78; on a reload the old rules stay in effect). httpdog signs every request with this secret, and the receiver verifies with the same secret.

Every request carries these two headers:

```http
X-Httpdog-Timestamp: 1794416274
X-Httpdog-Signature: sha256=1728aa567d7ef419cab2bf10fd5e1c764cdb608bd22d6f4d051b2043db19b8fe
```

- `X-Httpdog-Timestamp`: when the request was sent, in Unix seconds.
- `X-Httpdog-Signature`: `sha256=` followed by the lowercase hex of `HMAC-SHA256(secret, "<timestamp>.<request body>")`.

What the receiver must do, handling the body only when all of it passes:

1. **Compute the signature and compare it with the header.** Use the same secret and the same algorithm. The "request body" must be the raw bytes received; do not parse it as JSON and serialize it again. Compare with a constant-time function (such as Python's `hmac.compare_digest`), not `==`.
2. **Check that the timestamp is within a few minutes of now** (the example uses 5 minutes). This stops someone from capturing an old request and sending it again.
3. **Return 401 if either check fails**, and do not handle the body.

A few notes:

- **A signature is not encryption.** The body is still plain text; use an `https://` URL.
- **If the receiver does not verify, the signature does nothing**, and security still rests on keeping the URL secret.
- **Both clocks must be roughly right.** If the server times differ too much, requests are rejected as expired.
- **Retries are signed again.** Each attempt uses the current time, so a retry has a different timestamp and signature from the previous attempt.
- **Change the secret on both sides together.** On the httpdog side, restart after editing `secrets.env`.

## Receiver example

Python, standard library only. It verifies the signature and the timestamp and prints the alert only when both pass:

```python
import hashlib, hmac, json, os, time
from http.server import BaseHTTPRequestHandler, HTTPServer

SECRET = os.environ["HTTPDOG_WEBHOOK_SECRET"].encode()
MAX_AGE = 300  # seconds: reject a timestamp further than this from now

class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        timestamp = self.headers.get("X-Httpdog-Timestamp", "")
        signature = self.headers.get("X-Httpdog-Signature", "")

        expected = "sha256=" + hmac.new(
            SECRET, timestamp.encode() + b"." + body, hashlib.sha256
        ).hexdigest()
        fresh = timestamp.isdigit() and abs(time.time() - int(timestamp)) <= MAX_AGE
        if not (fresh and hmac.compare_digest(signature.encode(), expected.encode())):
            self.send_response(401)
            self.end_headers()
            return

        alert = json.loads(body)  # handle the body only after verification
        print(alert["instance"], alert["text"])
        self.send_response(204)
        self.end_headers()

HTTPServer(("127.0.0.1", 8080), Handler).serve_forever()
```

## Testing your receiver

No nginx needed. Replay the bundled `sample.log` with the example rule file and httpdog sends 3 real, signed alerts to your receiver:

```bash
export HTTPDOG_WEBHOOK_URL='https://<your receiver>/...'     # http://127.0.0.1:8080/ with the example above
export HTTPDOG_WEBHOOK_SECRET='<the same secret as the receiver>'

httpdog --stdin --config rules.example.yaml < sample.log
```

- **It works**: the terminal shows only 3 `ALERT` lines and the final summary, and the receiver gets 3 alerts whose `rule` is `sensitive-file-probe`, `slow-request` and `sensitive-file-probe`, with `instance` set to `web-01`.
- **It does not work**: each `ALERT` is followed by `webhook attempt N failed` and the reason, and httpdog gives up after 3 attempts.
- **A response other than 2xx also counts as a failure.** For example a 401 because signature verification failed; in that case check first that both sides have the same secret.

## When delivery fails

- Each request times out after 5 seconds.
- A failure is retried, for 3 attempts in total (1 second and 2 seconds apart).
- When all 3 fail, the alert is written to the system log and given up; it is not resent forever.
- A 2xx response counts as success. The response body is not read.
- Notifications are sent on a separate thread, so a slow webhook never holds up log intake. But when it is down for a long time and more than 1024 alerts are waiting, new alerts are dropped and written to the system log.
