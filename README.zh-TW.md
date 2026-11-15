# httpdog

[![build](https://github.com/guanting112/httpdog/actions/workflows/build.yml/badge.svg?branch=main)](https://github.com/guanting112/httpdog/actions/workflows/build.yml)
[![release](https://img.shields.io/github/v/release/guanting112/httpdog?sort=semver)](https://github.com/guanting112/httpdog/releases/latest)
[![license](https://img.shields.io/github/license/guanting112/httpdog)](LICENSE)
[![build provenance: attested](https://img.shields.io/badge/provenance-attested-brightgreen?logo=github)](https://github.com/guanting112/httpdog/attestations)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/guanting112/httpdog/badge)](https://scorecard.dev/viewer/?uri=github.com/guanting112/httpdog)
[![Linux amd64 and arm64](https://img.shields.io/badge/Linux-amd64%20%7C%20arm64-blue?logo=linux&logoColor=white)](#2-安裝到-ubuntu)

[English](README.md) | 繁體中文

盯著網站的 access log,看到可疑或異常的請求就通知你。

例如:

- 有人在掃 `/.env`、`/.git/config` 這類敏感檔案
- 同一個 IP 一分鐘內登入失敗 20 次
- 網站突然大量回 5xx,或某些頁面變得很慢

你把「什麼情況要通知」寫成規則檔,httpdog 一筆一筆比對 nginx / Apache 送來的 log,符合就發通知。

```
nginx / Apache ──每筆請求的 log──▶ httpdog(照規則比對)──▶ 通知:你的 webhook 網址,或系統 log
```

- **輕**:單一執行檔(約 4 MB),閒置記憶體約 5 MB,不需要資料庫或其他服務
- **好裝**:Release 附有 amd64 與 arm64 的執行檔,下載就能用,不用裝 Rust
- **不改網站設定也能用**:直接吃 nginx / Apache 預設的 log 格式
- **改規則不用重啟**:改完 `systemctl reload` 就生效;新規則寫錯會沿用舊的,監控不中斷
- **通知不會拖慢收 log**:webhook 不通也不影響比對

它只做「比對後通知」,不儲存 log、沒有查詢介面。要回頭查歷史 log,請用 Loki 或 Graylog 這類工具。

## 目錄

1. [先試跑看看](#1-先試跑看看)
2. [安裝到 Ubuntu](#2-安裝到-ubuntu)
3. [把 log 接進來](#3-把-log-接進來)
4. [寫規則](#4-寫規則)
5. [接收通知](#5-接收通知)
6. [日常維運](#6-日常維運)
7. [遇到問題](#7-遇到問題)

其他:[命令列](#命令列)、[更多文件](#更多文件)、[開發](#開發)

## 1. 先試跑看看

不需要 nginx,用附的 `sample.log` 重播一次,就能看到規則怎麼觸發。這一步要自己編譯,需要 Rust 1.85 以上。

```bash
cargo build --release

# 範例規則會把告警送到 webhook。還沒有接收端的話,先給假的網址與密鑰
export HTTPDOG_WEBHOOK_URL=http://127.0.0.1:9/dummy
export HTTPDOG_WEBHOOK_SECRET=dummy

./target/release/httpdog --check --config rules.example.yaml          # 檢查規則檔有沒有寫錯
./target/release/httpdog --stdin --config rules.example.yaml < sample.log
```

預期輸出:

```
httpdog started: 4 rule(s), reading from stdin
ALERT [httpdog] sensitive-file-probe | 203.0.113.7 tried to access /.env (status 404) | remote_addr=203.0.113.7
ALERT [httpdog] slow-request | /report/monthly took 7.31s
ALERT [httpdog] sensitive-file-probe | 192.0.2.44 tried to access /backup/db.sql (status 404) | remote_addr=192.0.2.44
done: 6 lines, 0 unparseable, 3 alerts fired
```

程式的輸出與 log 訊息都是英文。每一行 `ALERT` 就是一則通知。`sample.log` 裡第一個 IP 接著又探測了 `/.git/config`,但同一個 IP 剛通知過(還在冷卻時間內),所以沒有重複通知。

用假網址時,每則 `ALERT` 後面還會多幾行 `webhook attempt N failed`,整個過程約 9 秒,這是正常的:網址是假的,送不出去。

**想順便測試自己的 webhook 接收端**,把上面兩個環境變數換成真的網址與密鑰再跑一次,接收端會收到這 3 則告警,畫面上也不會有失敗的訊息。還沒有接收端可以先用 [docs/webhook.md](docs/webhook.md#receiver-example) 的範例在本機起一個。

## 2. 安裝到 Ubuntu

每個版本的 [Release](https://github.com/guanting112/httpdog/releases) 都附有編譯好的執行檔,下載後不用安裝 Rust,也不用管 Ubuntu 的版本。

```bash
# 1. 執行檔:從 Release 下載。ARCH 依機器選,用 uname -m 查:
#    x86_64 → amd64,aarch64 → arm64
ARCH=amd64
gh release download --repo guanting112/httpdog --pattern "httpdog-linux-$ARCH" --pattern SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS      # 應該顯示 httpdog-linux-...: OK
sudo install -m 755 "httpdog-linux-$ARCH" /usr/local/bin/httpdog
httpdog --version

# 2. 規則檔
sudo install -d /etc/httpdog
sudo install -m 644 rules.example.yaml /etc/httpdog/rules.yaml

# 3. 機密(webhook 的網址與密鑰),只有 root 讀得到
sudo install -m 600 /dev/null /etc/httpdog/secrets.env
sudoedit /etc/httpdog/secrets.env      # 寫入 HTTPDOG_WEBHOOK_URL=https://<你的接收端>/...
                                       # 以及 HTTPDOG_WEBHOOK_SECRET=<openssl rand -hex 32 產生的值>

# 4. 交給 systemd 管理
sudo install -m 644 deploy/httpdog.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now httpdog
systemctl status httpdog
```

補充:

- 第 1 步用的是 GitHub 的命令列工具 `gh`,要先 `gh auth login`。要指定版本就加上 tag,例如 `gh release download v0.1.0 --repo ...`。
- `SHA256SUMS` 只能發現下載過程中損壞的檔案。要確認執行檔真的是這個 repo 的 workflow 編出來的,可以驗證它的建置來源證明(有簽章):`gh attestation verify "httpdog-linux-$ARCH" -R guanting112/httpdog`。
- `rules.example.yaml` 和 `deploy/httpdog.service` 不在 Release 的附件裡,要從 repo 取得:`git clone`,或下載 Release 頁面上的 Source code 壓縮檔。
- 服務不需要事先建立帳號(unit 檔用了 `DynamicUser`)。`secrets.env` 由 systemd 以 root 讀取後才降權,服務本身碰不到那個檔案。
- 想自己編譯:在目標機器上 `cargo build --release`,再把第 1 步換成 `sudo install -m 755 target/release/httpdog /usr/local/bin/`。

## 3. 把 log 接進來

httpdog 看得懂兩種 log 格式,擇一即可:

| | 方式一:預設格式(`combined`) | 方式二:JSON |
|---|---|---|
| 網頁伺服器要改什麼 | nginx 加一行 | nginx 加一段 `log_format` |
| 規則能用的欄位 | 固定 12 個,沒有站台名稱(`host`)與處理時間(`request_time`) | 你決定,想比對什麼就加什麼 |
| 適合 | 先試試看;Apache | 長期使用;需要「慢請求」「依站台分組」這類規則 |

### 方式一:預設格式(combined)

nginx 與 Apache 預設的 access log 長這樣,httpdog 可以直接讀:

```
203.0.113.7 - - [11/Nov/2026:13:50:01 +0800] "GET /.env HTTP/1.1" 404 153 "-" "curl/8.5.0"
```

1. 規則檔最上層寫 `format: combined`。`examples/00-combined-starter.yaml` 是一份可以直接用的入門規則。
2. 先拿現有的 log 重播看看,什麼都不用改:

   ```bash
   httpdog --stdin --config examples/00-combined-starter.yaml < /var/log/nginx/access.log
   ```

3. 要即時監看,在 nginx 的 `http { }` 加一行,然後 `sudo nginx -t && sudo systemctl reload nginx`:

   ```nginx
   access_log syslog:server=127.0.0.1:5514,tag=nginx,nohostname;
   ```

Apache 的接法不一樣,見 [docs/log-format.md](docs/log-format.md#apache)。

### 方式二:JSON

把 `deploy/nginx-snippet.conf` 的內容放進 nginx 的 `http { }`,然後 `sudo nginx -t && sudo systemctl reload nginx`:

```nginx
log_format httpdog escape=json
    '{"remote_addr":"$remote_addr",'
    '"host":"$host",'
    '"request_method":"$request_method",'
    '"uri":"$uri",'
    '"request_uri":"$request_uri",'
    '"status":$status,'
    '"request_time":$request_time,'
    '"body_bytes_sent":$body_bytes_sent,'
    '"http_referer":"$http_referer",'
    '"http_user_agent":"$http_user_agent"}';

access_log syslog:server=127.0.0.1:5514,tag=nginx,nohostname httpdog;
```

**規則裡的欄位名稱就是這裡的 JSON key**。想比對別的東西,在 `log_format` 加一個 key 就能在規則裡用。

### 兩種方式都要注意

- 原本寫檔的 `access_log` 可以保留,同一層可以有多行。
- 站台在 Cloudflare 或其他反向代理後面時,要先設定 nginx 的 `real_ip`。否則每筆 log 的來源 IP 都是代理的 IP,「每個 IP 各算各的」會變成所有人算在一起。

每個欄位的內容、型別與注意事項見 [docs/log-format.md](docs/log-format.md)。

## 4. 寫規則

規則檔是一個 YAML 檔(也可以用 JSON),完整範例在 `rules.example.yaml`。下面這份只有一條規則:**同一個 IP 在 60 秒內登入失敗 20 次就通知**。

```yaml
instance: web-01                  # 這一台 httpdog 的名稱,通知裡會帶著

notifiers:                        # 通知可以送去哪些地方
  console:
    type: stderr                  # 寫到系統 log,測試規則時先用這個
  ops:
    type: webhook                 # 送到你自己的接收端
    url_env: HTTPDOG_WEBHOOK_URL
    secret_env: HTTPDOG_WEBHOOK_SECRET

rules:
  - id: login-bruteforce          # 規則的名稱
    when:                         # 條件:以下三項都成立
      all:
        - { field: uri, op: prefix, value: /login }
        - { field: request_method, op: eq, value: POST }
        - { field: status, op: in, value: [401, 403] }
    group_by: remote_addr         # 每個 IP 各算各的
    threshold: { count: 20, window: 60s }   # 60 秒內累積 20 次才通知
    cooldown: 10m                 # 通知後,同一個 IP 10 分鐘內不再通知
    notify: [console, ops]        # 要送到哪幾個 notifier
    message: "{remote_addr} repeated login failures"
```

一條規則由這幾個部分組成:

| 鍵 | 用途 |
|---|---|
| `when` | 條件。每一項是「log 的哪個欄位、怎麼比、跟什麼比」,可以用 `all`(全部成立)、`any`(任一成立)、`not`(反向)組合 |
| `threshold` | 門檻:一段時間內累積幾次才通知。不寫就是命中一筆就通知 |
| `group_by` | 分組:門檻與冷卻依這個欄位分開計算,例如每個 IP 各算各的 |
| `cooldown` | 冷卻:通知過後,同一組在這段時間內不再通知。不寫是 1 分鐘 |
| `notify` | 要送到哪些 notifier |
| `message` | 通知的文字,`{欄位名}` 會換成那筆 log 的值 |

比對的方式有相等、開頭、結尾、包含、正規表示式、數值大小等。完整的清單與細節見 [docs/rules.md](docs/rules.md)。

### 現成的規則範本

`examples/` 下有 6 份規則檔,每份都是可以直接載入的完整設定,開頭的註解寫了適用對象與套用前要改的地方。

| 檔案 | 情境 | 抓什麼 |
|---|---|---|
| `00-combined-starter.yaml` | 入門(`combined` 格式) | 不改 log 格式就能用的 8 條通用規則。只寫到系統 log,不需要 webhook |
| `01-security-probe.yaml` | 資安探測 | 敏感檔案被掃或被讀走、目錄穿越、SQL injection 試探、掃描工具、大量 404 |
| `02-auth-abuse.yaml` | 登入與帳號濫用 | 暴力破解、撞庫、濫發重設密碼、大量註冊、API 驗證失敗、持續被限流 |
| `03-site-health.yaml` | 站台健康 | 5xx 暴增、後端無回應、單站錯誤偏高、慢請求、健康檢查失敗、用戶端提早斷線 |
| `04-traffic-surge.yaml` | 流量異常 | 整站尖峰、單一 IP 灌流量、單一端點被集中打、腳本與爬蟲、超大回應 |
| `05-critical-endpoints.yaml` | 關鍵業務路徑 | 結帳出錯、金流回呼失敗或過慢、未知來源進後台、後台刪除操作、API 用戶端錯誤暴增 |

範本裡的門檻與路徑都是示意,要換成你站台實際的數字與路徑。套用前的注意事項見 [docs/rules.md](docs/rules.md#using-the-rule-templates)。

## 5. 接收通知

通知有兩個去處,在規則檔的 `notifiers` 設定:

- **`stderr`**:寫到系統 log,用 `journalctl -u httpdog` 看。測試新規則時先用這個。
- **`webhook`**:httpdog 把告警以 JSON 用 HTTP POST 送到你指定的網址,由你自己的程式接收,再決定要存起來、轉發還是顯示。

webhook 收到的內容長這樣:

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

要知道的三件事:

1. **一定要設密鑰**(`secret_env`),沒設就不能啟動。httpdog 用它替每個請求簽章,你的接收端用同一把密鑰驗證,才能確定請求真的是 httpdog 發的,不是別人偽造的。
2. **`instance` 用來分辨是哪一台發的**。多台 httpdog 送到同一個接收端時,在各自的規則檔寫上不同的 `instance`;沒寫的話是 `unknown`。
3. **送不出去會重試**:逾時 5 秒,總共試 3 次,都失敗就把告警寫進系統 log 後放棄。

每個欄位的說明、簽章怎麼驗證、可以直接拿去改的接收端範例,見 [docs/webhook.md](docs/webhook.md)。

## 6. 日常維運

```bash
journalctl -u httpdog -f            # 即時看 log(告警、重試、reload 的結果都在這)
sudo systemctl reload httpdog       # 套用新規則
sudo systemctl restart httpdog      # 改了 listen 或 secrets.env 之後
```

**改規則的建議流程**:先檢查,再拿最近的 log 重播看會觸發幾次,最後才 reload。

```bash
# 1. 檢查規則檔。secrets.env 只有 root 讀得到,所以要先載入環境變數才檢查得過
sudo bash -c 'set -a; . /etc/httpdog/secrets.env; set +a
  httpdog --check --config /etc/httpdog/rules.yaml'

# 2. 拿最近的 log 重播,結束時會印出觸發了幾則(規則檔用到 webhook 的話,同樣要先載入環境變數)
httpdog --stdin --config new-rules.yaml < access.log

# 3. 套用
sudo systemctl reload httpdog
journalctl -u httpdog -n 5          # 確認出現 rules reloaded
```

- 第 1 步是用 shell 讀 `secrets.env`,所以網址裡有 `&` 等特殊字元時,檔案裡的值要用單引號包起來。
- 第 2 步重播時,規則命中的告警會真的送出去。不想打擾接收端的話,先把新規則的 `notify` 改成只有 `console`,確認沒問題再改回來。

**手動送一筆測試**,確認整條路是通的(JSON 格式,搭配有比對 `/.env` 的規則):

```bash
echo '<190>Nov 11 12:00:00 nginx: {"remote_addr":"192.0.2.1","request_method":"GET","uri":"/.env","status":404}' \
  | nc -u -w1 127.0.0.1 5514
```

## 7. 遇到問題

| 現象 | 可能原因 |
|---|---|
| 完全沒有告警 | nginx 的 `server=` 與規則檔的 `listen` 不一致;或 log 的格式與規則檔的 `format` 對不上(結束時的統計會顯示很多行 `unparseable`) |
| 規則應該要中卻沒中 | 欄位名稱與 `log_format` 的 key 不一致;或該用 `request_uri` 卻用了 `uri`(見 [docs/log-format.md](docs/log-format.md)) |
| 只通知一次就沒了 | 還在 `cooldown` 內,預設是 1 分鐘 |
| 啟動後馬上停止、不再重啟 | 規則檔有錯(結束碼 78),看 `journalctl -u httpdog -n 20` |
| 同一個 IP 觸發所有人的門檻 | nginx 在代理後面,來源 IP 都是代理的 IP,要設定 `real_ip` |
| log 出現 `falling behind, ... dropped so far`,或收到 `overload` 告警 | 流量超過處理速度,檢查是否有效能很差的 regex。log 裡的數字是啟動以來的累計,告警裡的是上一則告警之後的數量 |

## 命令列

```
httpdog [--config rules.yaml] [--check] [--stdin] [--version]

  -c, --config <檔案>  規則檔,預設 ./rules.yaml
      --check          只驗證規則檔,不啟動
      --stdin          從 stdin 讀 log,不開 UDP
  -V, --version        印出版本(例如 httpdog 0.1.0)後結束
```

`--stdin` 有兩個用途:

1. **重播舊 log 測規則**:`httpdog --stdin --config new-rules.yaml < access.log`,結束時會印出觸發了幾則。
2. **接在別的程式後面**:例如 Apache 的管線,或 rsyslog 的 `omprog`(由 rsyslog 收 syslog 並啟動 httpdog,此時不需要 unit 檔)。

| 訊號 | 行為 |
|---|---|
| `SIGHUP` | 重新載入規則檔(`systemctl reload` 送的就是這個)。新檔有錯就沿用舊規則並記錄原因 |
| `SIGTERM` / `SIGINT` | 把還沒送出的通知送完後結束 |

| 結束碼 | 意義 |
|---|---|
| 0 | 正常結束 |
| 1 | 無法監聽 UDP 位址 |
| 2 | 命令列參數錯誤 |
| 78 | 規則檔錯誤。此時 systemd 不會自動重啟(unit 檔設了 `RestartPreventExitStatus=78`) |

## 更多文件

以下文件目前只有英文版。

| 文件 | 內容 |
|---|---|
| [docs/rules.md](docs/rules.md) | 規則檔的完整說明:每個鍵、所有比對方式、比對的細節、處理不及時的告警、套用範本的注意事項 |
| [docs/log-format.md](docs/log-format.md) | 兩種 log 格式各有哪些欄位、Apache 的接法、怎麼加欄位 |
| [docs/webhook.md](docs/webhook.md) | webhook 每個欄位的說明、簽章怎麼驗證、接收端範例 |
| [docs/limits.md](docs/limits.md) | 已知限制,以及哪些部分實際驗證過、哪些還沒有 |
| [docs/development.md](docs/development.md) | 開發與發版:程式結構、測試、CI |
| [CHANGELOG.md](CHANGELOG.md) | 各版本的變動 |
| [SECURITY.md](SECURITY.md) | 怎麼回報安全性漏洞,以及哪些情況算漏洞 |
| [CONTRIBUTING.md](CONTRIBUTING.md) | 怎麼回報問題或提出修改,以及設計原則 |

## 開發

```bash
make ci             # 格式檢查、clippy、單元測試、範例規則檔的檢查、煙霧測試,提交前跑一次
make fuzz           # 對 log 解析器與規則引擎做模糊測試,所有 target 同時跑,各 60 秒(FUZZTIME=300 可以跑久一點)
make                # 列出所有目標
```

細節見 [docs/development.md](docs/development.md)。

## 授權

以 MIT 授權釋出,見 [LICENSE](LICENSE)。
