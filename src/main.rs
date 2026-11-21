//! httpdog: reads nginx / Apache access logs (syslog over a unix socket or UDP, or stdin),
//! matches them against a rule file and sends alerts.
//!
//! Architecture:
//!   input thread (socket / stdin) --lines--> main loop (rule engine) --alerts--> notifier thread (webhook)
//!
//! The main loop wakes at least once a second to check signals, feed the systemd watchdog and sweep expired state.

// The same rule as in src/lib.rs: no unsafe code in the binary either
#![forbid(unsafe_code)]

use httpdog::{
    config::{Config, Listen},
    engine::Engine,
    format, notify,
};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use std::{
    fs,
    io::{self, BufRead, ErrorKind},
    net::{SocketAddr, ToSocketAddrs, UdpSocket},
    os::{
        fd::OwnedFd,
        unix::{
            fs::{FileTypeExt, PermissionsExt},
            net::UnixDatagram,
        },
    },
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

fn load(path: &Path) -> Result<(Engine, Listen), String> {
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
    let mut held_by_systemd = false;
    if args.stdin {
        thread::spawn(move || read_stdin(line_tx));
        eprintln!(
            "httpdog started: {} rule(s), reading from stdin",
            engine.rule_count()
        );
    } else {
        // Under deploy/httpdog.socket systemd holds the socket and passes it at every start, so a restart of
        // httpdog keeps the same socket. Started any other way, httpdog opens the socket itself
        let input = match Input::from_systemd(&listen) {
            Ok(Some(input)) => {
                held_by_systemd = true;
                input
            }
            Ok(None) => Input::open(&listen).unwrap_or_else(|e| {
                eprintln!("cannot listen on {listen}: {e}");
                exit(1);
            }),
            // The unit file and the rule file disagree: a restart would not change that
            Err(e) => {
                eprintln!("config error: {e}");
                exit(EXIT_CONFIG);
            }
        };
        eprintln!(
            "httpdog started: {} rule(s), listening on {listen}{}",
            engine.rule_count(),
            if held_by_systemd {
                " (socket held by systemd)"
            } else {
                ""
            }
        );
        if let Some(warning) = input.warning() {
            eprintln!("{warning}");
        }
        let dropped = dropped.clone();
        thread::spawn(move || read_socket(input, line_tx, &dropped));
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
                // systemd holds the socket and passes the same one at every start, and httpdog does not start
                // when `listen` names another. Taking these rules would leave a rule file that works until the
                // next restart or reboot and then keeps httpdog down, so it is refused now, while someone is looking
                Ok((_, new_listen)) if held_by_systemd && new_listen != listen => eprintln!(
                    "reload failed, keeping the old rules: listen is now {new_listen} in the rule file, \
                     but systemd holds the socket {listen} and a reload cannot change it. To move to another \
                     address, change ListenDatagram= in httpdog.socket to the same one and restart \
                     httpdog.socket; otherwise change listen back"
                ),
                Ok((new_engine, new_listen)) => {
                    engine = new_engine;
                    eprintln!(
                        "rules reloaded: {} rule(s) (counters and cooldowns reset)",
                        engine.rule_count()
                    );
                    // The socket is opened once at startup. Say so, or a changed listen looks applied when it is not
                    if !args.stdin && new_listen != listen {
                        eprintln!(
                            "listen is now {new_listen} in the rule file, but a reload does not apply it: \
                             still listening on {listen} until the next restart"
                        );
                    }
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

/// The socket logs arrive on, one syslog message per datagram
enum Input {
    Unix(UnixDatagram),
    Udp(UdpSocket),
}

impl Input {
    fn open(listen: &Listen) -> Result<Input, String> {
        match listen {
            Listen::Unix(path) => bind_unix(path).map(Input::Unix),
            Listen::Udp(addr) => UdpSocket::bind(addr)
                .map(Input::Udp)
                .map_err(|e| e.to_string()),
        }
    }

    /// The socket systemd passed to this process, if it passed one: deploy/httpdog.socket makes it do so.
    /// The socket has to be the one `listen` in the rule file names. The web server is pointed at what the
    /// rule file says, so with any other socket httpdog would run and receive nothing
    fn from_systemd(listen: &Listen) -> Result<Option<Input>, String> {
        let mut fds = sd_listen_fds::get().map_err(|e| format!("systemd socket: {e}"))?;
        let fd = match fds.len() {
            0 => return Ok(None),
            1 => fds.remove(0).1,
            n => {
                return Err(format!(
                    "systemd passed {n} sockets and httpdog takes one: \
                     keep a single ListenDatagram= line in httpdog.socket"
                ));
            }
        };
        let input = match listen {
            Listen::Unix(_) => Input::Unix(fd.into()),
            Listen::Udp(_) => Input::Udp(fd.into()),
        };
        input.check_passed(listen).map_err(|e| {
            format!(
                "the socket systemd passed is not the one the rule file names, {listen}: {e}. \
                 Make ListenDatagram= in httpdog.socket and listen in the rule file the same"
            )
        })?;
        Ok(Some(input))
    }

    /// Nothing but its number is known about a passed socket, so ask the socket itself: is it bound to the
    /// address in `listen`, and does it take datagrams
    fn check_passed(&self, listen: &Listen) -> Result<(), String> {
        // A socket of the other family cannot say its address in this one: that is how the wrong kind shows
        match (self, listen) {
            (Input::Unix(sock), Listen::Unix(path)) => {
                let addr = sock
                    .local_addr()
                    .map_err(|_| "it is not a unix socket".to_string())?;
                if addr.as_pathname() != Some(path.as_path()) {
                    return Err(match addr.as_pathname() {
                        Some(p) => format!("it is unix:{}", p.display()),
                        None => "it has no path".into(),
                    });
                }
            }
            (Input::Udp(sock), Listen::Udp(addr)) => {
                let local = sock
                    .local_addr()
                    .map_err(|_| "it is not a UDP socket".to_string())?;
                let mut wanted = addr
                    .to_socket_addrs()
                    .map_err(|e| format!("cannot resolve {addr}: {e}"))?;
                if !wanted.any(|a| a == local) {
                    return Err(format!("it is {local}"));
                }
            }
            _ => return Err("it is another kind of socket".into()),
        }
        // A stream socket (ListenStream=) has an address too, but every receive on it fails
        match self.peek_now() {
            Err(e) if e.kind() != ErrorKind::WouldBlock => {
                Err(format!("it does not take datagrams ({e})"))
            }
            _ => Ok(()),
        }
    }

    /// Look at the next datagram without waiting for one and without taking it out of the queue.
    /// On a socket that takes datagrams the answer is the datagram or "nothing yet" (WouldBlock)
    fn peek_now(&self) -> io::Result<usize> {
        // UnixDatagram::peek is not in stable Rust yet (rust-lang/rust#76923). UdpSocket::peek makes the same
        // recv(MSG_PEEK) call, and a file descriptor does not care which Rust type holds it
        let probe = match self {
            Input::Unix(sock) => UdpSocket::from(OwnedFd::from(sock.try_clone()?)),
            Input::Udp(sock) => sock.try_clone()?,
        };
        // The copy shares this setting with the socket itself, so it has to be put back:
        // the input thread waits for datagrams
        probe.set_nonblocking(true)?;
        let peeked = probe.peek(&mut [0u8; 1]);
        probe.set_nonblocking(false)?;
        peeked
    }

    /// UDP has no way to limit who sends, so using it is worth a line in the log at every start
    fn warning(&self) -> Option<String> {
        match self {
            Input::Unix(_) => None,
            Input::Udp(sock) => sock.local_addr().ok().map(udp_warning),
        }
    }

    fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Input::Unix(sock) => sock.recv(buf),
            Input::Udp(sock) => sock.recv_from(buf).map(|(n, _)| n),
        }
    }
}

/// Create the unix socket that receives logs: what httpdog does when systemd does not pass it one (started by
/// hand, or by a unit file without deploy/httpdog.socket). Sending to the socket needs write permission on the
/// socket file, which is given to the owner and the group only (0660). The group is the one httpdog runs as,
/// so nobody else can send until someone gives the file to the web server's group.
fn bind_unix(path: &Path) -> Result<UnixDatagram, String> {
    // A socket file left behind by an earlier run (after SIGKILL, say) makes bind fail, so it has to go first.
    // Remove it only when nothing receives on it any more, and never remove anything that is not a socket
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            let probe = UnixDatagram::unbound().map_err(|e| e.to_string())?;
            match probe.connect(path) {
                Ok(()) => return Err("another process is already listening on it".into()),
                Err(e) if e.kind() == ErrorKind::ConnectionRefused => {
                    fs::remove_file(path).map_err(|e| {
                        format!("cannot remove the socket left by an earlier run: {e}")
                    })?;
                }
                Err(e) => {
                    return Err(format!(
                        "a socket is already there and cannot be checked: {e}"
                    ));
                }
            }
        }
        Ok(_) => return Err("the path exists and is not a socket".into()),
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }

    let sock = UnixDatagram::bind(path).map_err(|e| match (e.kind(), path.parent()) {
        (ErrorKind::NotFound, Some(dir)) => format!(
            "the directory {} does not exist. Under systemd, deploy/httpdog.socket creates /run/httpdog \
             and the socket in it; when starting httpdog another way, create the directory first or set \
             listen to another path",
            dir.display()
        ),
        _ => e.to_string(),
    })?;
    // Until this line the mode comes from the umask, and the usual 022 already leaves group and others
    // without write permission
    fs::set_permissions(path, fs::Permissions::from_mode(0o660))
        .map_err(|e| format!("cannot set the permissions of the socket: {e}"))?;
    Ok(sock)
}

fn udp_warning(addr: SocketAddr) -> String {
    let who = if addr.ip().is_loopback() {
        "any program on this machine"
    } else {
        "any host that can reach this address, and any program on this machine,"
    };
    format!(
        "warning: listening on UDP {addr}: {who} can send logs here and make httpdog raise false alerts. \
         A unix socket limits who can send: use `listen: unix:/run/httpdog/httpdog.sock` \
         (the default when listen is left out) and point the web server at the same path"
    )
}

/// `unreported` is what the main loop uses to raise overload alerts; it is reset when the main loop collects it.
/// A separate total since startup is kept here for the log line.
fn read_socket(input: Input, tx: SyncSender<String>, unreported: &AtomicU64) {
    let mut buf = vec![0u8; 65535];
    let mut dropped = 0u64;
    loop {
        let n = match input.recv(&mut buf) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("receive error: {e}");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory for one test. The name is short because a socket path holds about 100 bytes
    /// and the temporary directory on macOS is already long
    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hd-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).unwrap();
        dir
    }

    fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn unix_socket_receives_and_is_closed_to_others() {
        let dir = test_dir("recv");
        let path = dir.join("s");
        let input = Input::open(&Listen::Unix(path.clone())).unwrap();
        assert!(input.warning().is_none());
        assert_eq!(mode(&path), 0o660);

        let sender = UnixDatagram::unbound().unwrap();
        sender.send_to(b"<190>nginx: {}", &path).unwrap();
        let mut buf = [0u8; 64];
        let n = input.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"<190>nginx: {}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unix_socket_left_by_an_earlier_run_is_replaced() {
        let dir = test_dir("stale");
        let path = dir.join("s");
        drop(bind_unix(&path).unwrap());
        assert!(path.exists());

        let sock = bind_unix(&path).unwrap();
        UnixDatagram::unbound()
            .unwrap()
            .send_to(b"x", &path)
            .unwrap();
        assert_eq!(sock.recv(&mut [0u8; 8]).unwrap(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unix_socket_in_use_is_not_taken_over() {
        let dir = test_dir("live");
        let path = dir.join("s");
        let first = bind_unix(&path).unwrap();
        assert!(bind_unix(&path).unwrap_err().contains("already listening"));

        // The first one still receives: the refused attempt must not have removed its file
        UnixDatagram::unbound()
            .unwrap()
            .send_to(b"x", &path)
            .unwrap();
        assert_eq!(first.recv(&mut [0u8; 8]).unwrap(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unix_socket_never_replaces_an_ordinary_file() {
        let dir = test_dir("file");
        let path = dir.join("s");
        fs::write(&path, "keep me").unwrap();
        assert!(bind_unix(&path).unwrap_err().contains("not a socket"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "keep me");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unix_socket_needs_its_directory() {
        let dir = test_dir("nodir");
        let err = bind_unix(&dir.join("missing/s")).unwrap_err();
        assert!(err.contains("does not exist"), "{err}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn passed_unix_socket_must_be_the_one_in_listen() {
        let dir = test_dir("passed");
        let path = dir.join("s");
        let input = Input::Unix(UnixDatagram::bind(&path).unwrap());
        assert_eq!(input.check_passed(&Listen::Unix(path.clone())), Ok(()));

        // A datagram already waiting is left in the queue, and the socket still waits for the next one
        let sender = UnixDatagram::unbound().unwrap();
        sender.send_to(b"first", &path).unwrap();
        assert_eq!(input.check_passed(&Listen::Unix(path.clone())), Ok(()));
        let mut buf = [0u8; 16];
        let n = input.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"first");
        let Input::Unix(sock) = &input else {
            unreachable!()
        };
        sock.set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let started = Instant::now();
        assert!(input.recv(&mut buf).is_err());
        assert!(started.elapsed() >= Duration::from_millis(40));

        let err = input
            .check_passed(&Listen::Unix(dir.join("other")))
            .unwrap_err();
        assert_eq!(err, format!("it is unix:{}", path.display()));
        let err = input
            .check_passed(&Listen::Udp("127.0.0.1:5514".into()))
            .unwrap_err();
        assert_eq!(err, "it is another kind of socket");

        let unnamed = Input::Unix(UnixDatagram::unbound().unwrap());
        let err = unnamed.check_passed(&Listen::Unix(path)).unwrap_err();
        assert_eq!(err, "it has no path");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn passed_stream_socket_is_refused() {
        // What ListenStream= in place of ListenDatagram= would pass: the right path, the wrong kind of socket
        let dir = test_dir("stream");
        let path = dir.join("s");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let input = Input::Unix(OwnedFd::from(listener).into());
        let err = input.check_passed(&Listen::Unix(path)).unwrap_err();
        assert!(err.starts_with("it does not take datagrams"), "{err}");

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let input = Input::Udp(OwnedFd::from(listener).into());
        let err = input
            .check_passed(&Listen::Udp(addr.to_string()))
            .unwrap_err();
        assert!(err.starts_with("it does not take datagrams"), "{err}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn passed_udp_socket_must_be_the_one_in_listen() {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = sock.local_addr().unwrap();
        let input = Input::Udp(sock);
        assert_eq!(input.check_passed(&Listen::Udp(addr.to_string())), Ok(()));
        // The warning about UDP does not depend on who opened the socket
        assert!(input.warning().unwrap().contains("listening on UDP"));

        let err = input
            .check_passed(&Listen::Udp("127.0.0.1:1".into()))
            .unwrap_err();
        assert_eq!(err, format!("it is {addr}"));
        let err = input
            .check_passed(&Listen::Unix("/run/httpdog/httpdog.sock".into()))
            .unwrap_err();
        assert_eq!(err, "it is another kind of socket");
    }

    #[test]
    fn passed_socket_of_the_other_family_is_refused() {
        // from_systemd wraps the socket as what `listen` says it should be, whatever systemd passed
        let dir = test_dir("family");
        let path = dir.join("s");
        let unix = UnixDatagram::bind(&path).unwrap();
        let as_udp = Input::Udp(OwnedFd::from(unix).into());
        let err = as_udp
            .check_passed(&Listen::Udp("127.0.0.1:5514".into()))
            .unwrap_err();
        assert_eq!(err, "it is not a UDP socket");

        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let as_unix = Input::Unix(OwnedFd::from(udp).into());
        let err = as_unix.check_passed(&Listen::Unix(path)).unwrap_err();
        assert_eq!(err, "it is not a unix socket");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn udp_always_warns() {
        let input = Input::open(&Listen::Udp("127.0.0.1:0".into())).unwrap();
        let local = input.warning().unwrap();
        assert!(local.starts_with("warning: listening on UDP 127.0.0.1:"));
        assert!(local.contains("any program on this machine can send"));
        assert!(!local.contains("any host"));

        let exposed = udp_warning("0.0.0.0:5514".parse().unwrap());
        assert!(exposed.contains("any host that can reach this address"));
        let exposed = udp_warning("192.0.2.10:5514".parse().unwrap());
        assert!(exposed.contains("any host that can reach this address"));
        assert!(
            udp_warning("[::1]:5514".parse().unwrap()).contains("any program on this machine can")
        );
    }
}
