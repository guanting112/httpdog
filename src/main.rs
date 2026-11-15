//! httpdog: reads nginx / Apache access logs (syslog UDP or stdin), matches them against a rule file and sends alerts.
//!
//! Architecture:
//!   input thread (UDP / stdin) --lines--> main loop (rule engine) --alerts--> notifier thread (webhook)
//!
//! The main loop wakes at least once a second to check signals, feed the systemd watchdog and sweep expired state.

use httpdog::{config::Config, engine::Engine, format, notify};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use std::{
    io::BufRead,
    net::UdpSocket,
    path::{Path, PathBuf},
    process::exit,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, RecvTimeoutError, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

/// EX_CONFIG from sysexits.h. The unit file pairs it with RestartPreventExitStatus=78,
/// so systemd does not restart forever when the config is broken
const EXIT_CONFIG: i32 = 78;
const LINE_QUEUE: usize = 8192;
const SWEEP_EVERY: Duration = Duration::from_secs(60);

struct Args {
    config: PathBuf,
    check: bool,
    stdin: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        config: PathBuf::from("rules.yaml"),
        check: false,
        stdin: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-c" | "--config" => match it.next() {
                Some(path) => args.config = path.into(),
                None => usage("--config needs a file path"),
            },
            "--check" => args.check = true,
            "--stdin" => args.stdin = true,
            "-h" | "--help" => usage(""),
            // The version is a query result, not a log line, so it goes to stdout and `httpdog --version` can be piped
            "-V" | "--version" => {
                println!("httpdog {}", env!("CARGO_PKG_VERSION"));
                exit(0);
            }
            other => usage(&format!("unknown argument: {other}")),
        }
    }
    args
}

fn usage(err: &str) -> ! {
    if !err.is_empty() {
        eprintln!("{err}\n");
    }
    eprintln!(
        "usage: httpdog [--config rules.yaml] [--check] [--stdin] [--version]\n\
         \n\
         \x20 -c, --config <file>  rule file (.yaml / .yml / .json), default ./rules.yaml\n\
         \x20     --check          validate the rule file and exit\n\
         \x20     --stdin          read logs from stdin (for rsyslog omprog, or to replay old logs against rules)\n\
         \x20 -V, --version        print the version and exit"
    );
    exit(if err.is_empty() { 0 } else { 2 });
}

fn load(path: &Path) -> Result<(Engine, String), String> {
    let cfg = Config::load(path)?;
    let listen = cfg.listen.clone();
    Ok((Engine::from_config(cfg)?, listen))
}

fn main() {
    let args = parse_args();

    let (mut engine, listen) = match load(&args.config) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("config error: {e}");
            exit(EXIT_CONFIG);
        }
    };
    if args.check {
        eprintln!("OK: {} rule(s)", engine.rule_count());
        return;
    }

    // Signal handlers only set flags; the main loop does the real work
    let term = Arc::new(AtomicBool::new(false));
    let hup = Arc::new(AtomicBool::new(false));
    for sig in [SIGTERM, SIGINT] {
        signal_hook::flag::register(sig, term.clone()).expect("failed to register signal handler");
    }
    signal_hook::flag::register(SIGHUP, hup.clone()).expect("failed to register signal handler");

    let (line_tx, line_rx) = mpsc::sync_channel::<String>(LINE_QUEUE);
    // Lines dropped by the input thread that the main loop has not collected yet. stdin mode never drops, so it stays 0
    let dropped = Arc::new(AtomicU64::new(0));
    if args.stdin {
        thread::spawn(move || read_stdin(line_tx));
        eprintln!(
            "httpdog started: {} rule(s), reading from stdin",
            engine.rule_count()
        );
    } else {
        let sock = UdpSocket::bind(&listen).unwrap_or_else(|e| {
            eprintln!("cannot listen on UDP {listen}: {e}");
            exit(1);
        });
        let dropped = dropped.clone();
        thread::spawn(move || read_udp(sock, line_tx, &dropped));
        eprintln!(
            "httpdog started: {} rule(s), listening on UDP {listen}",
            engine.rule_count()
        );
    }

    let (alert_tx, notifier) = notify::spawn();
    sd_notify("READY=1");

    let (mut lines, mut skipped, mut fired) = (0u64, 0u64, 0u64);
    let mut last_tick = Instant::now();
    let mut last_sweep = Instant::now();

    loop {
        if term.load(Ordering::Relaxed) {
            eprintln!(
                "termination signal received, sending the remaining notifications before exit"
            );
            break;
        }
        if hup.swap(false, Ordering::Relaxed) {
            // A bad new rule file keeps the old rules: monitoring must not stop because of a config mistake
            match load(&args.config) {
                Ok((new_engine, _)) => {
                    engine = new_engine;
                    eprintln!(
                        "rules reloaded: {} rule(s) (counters and cooldowns reset)",
                        engine.rule_count()
                    );
                }
                Err(e) => eprintln!("reload failed, keeping the old rules: {e}"),
            }
        }

        match line_rx.recv_timeout(Duration::from_secs(1)) {
            Ok(line) => {
                lines += 1;
                match format::parse(engine.format(), &line) {
                    Some((event, raw)) => {
                        for alert in engine.process(&event, raw, Instant::now()) {
                            fired += 1;
                            enqueue(&alert_tx, alert);
                        }
                    }
                    None => skipped += 1,
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break, // stdin reached EOF
        }

        // The watchdog ping must come from the main loop, so systemd can tell when the loop hangs
        let now = Instant::now();
        if now.duration_since(last_tick) >= Duration::from_secs(1) {
            last_tick = now;
            sd_notify("WATCHDOG=1");
            // Turn the count dropped by the input thread into an alert. Call it every second (even with 0),
            // so a count accumulated during the cooldown gets reported once the cooldown ends
            let n = dropped.swap(0, Ordering::Relaxed);
            if let Some(alert) = engine.report_dropped(n, now) {
                fired += 1;
                enqueue(&alert_tx, alert);
            }
        }
        if now.duration_since(last_sweep) >= SWEEP_EVERY {
            last_sweep = now;
            engine.sweep(now);
        }
    }

    sd_notify("STOPPING=1");
    drop(alert_tx); // Close the queue; the notifier thread drains it and exits
    let _ = notifier.join();
    eprintln!("done: {lines} lines, {skipped} unparseable, {fired} alerts fired");
}

/// Hand an alert to the notifier thread. Uses try_send: when the notifier is backed up,
/// drop the alert rather than block the main loop
fn enqueue(tx: &SyncSender<notify::Alert>, alert: notify::Alert) {
    if let Err(TrySendError::Full(a)) = tx.try_send(alert) {
        eprintln!("notification queue full, dropped: {}", a.text);
    }
}

/// `unreported` is what the main loop uses to raise overload alerts; it is reset when the main loop collects it.
/// A separate total since startup is kept here for the log line.
fn read_udp(sock: UdpSocket, tx: SyncSender<String>, unreported: &AtomicU64) {
    let mut buf = vec![0u8; 65535];
    let mut dropped = 0u64;
    loop {
        let n = match sock.recv_from(&mut buf) {
            Ok((n, _)) => n,
            Err(e) => {
                eprintln!("UDP receive error: {e}");
                continue;
            }
        };
        let line = String::from_utf8_lossy(&buf[..n]).into_owned();
        match tx.try_send(line) {
            Ok(()) => {}
            // When the main loop cannot keep up, drop logs rather than let memory grow without bound
            Err(TrySendError::Full(_)) => {
                dropped += 1;
                unreported.fetch_add(1, Ordering::Relaxed);
                if dropped.is_power_of_two() {
                    eprintln!("falling behind, {dropped} log lines dropped so far");
                }
            }
            Err(TrySendError::Disconnected(_)) => return,
        }
    }
}

fn read_stdin(tx: SyncSender<String>) {
    for line in std::io::stdin().lock().lines() {
        match line {
            // stdin uses a blocking send: replaying old logs must not lose a single line
            Ok(line) => {
                if tx.send(line).is_err() {
                    return;
                }
            }
            Err(e) => {
                eprintln!("stdin read error: {e}");
                return;
            }
        }
    }
    // tx is dropped when this function returns; the main loop sees Disconnected and exits
}

/// systemd's notify protocol: write one string to $NOTIFY_SOCKET, no extra crate needed.
/// Does nothing when not started by systemd (the variable is unset).
#[cfg(unix)]
fn sd_notify(state: &str) {
    use std::os::unix::net::UnixDatagram;

    let Some(path) = std::env::var_os("NOTIFY_SOCKET") else {
        return;
    };
    let Ok(sock) = UnixDatagram::unbound() else {
        return;
    };
    let bytes = path.as_encoded_bytes();

    if let Some(_name) = bytes.strip_prefix(b"@") {
        // A leading @ means a Linux abstract socket (used by older systemd)
        #[cfg(target_os = "linux")]
        {
            use std::os::linux::net::SocketAddrExt;
            use std::os::unix::net::SocketAddr;
            if let Ok(addr) = SocketAddr::from_abstract_name(_name) {
                let _ = sock.send_to_addr(state.as_bytes(), &addr);
            }
        }
    } else {
        let _ = sock.send_to(state.as_bytes(), &path);
    }
}

#[cfg(not(unix))]
fn sd_notify(_state: &str) {}
