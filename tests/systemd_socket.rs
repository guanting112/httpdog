//! httpdog started the way systemd starts it under deploy/httpdog.socket: the socket already exists,
//! it arrives as file descriptor 3, and LISTEN_PID / LISTEN_FDS announce it.
//!
//! Linux only, like the hand-over itself. No systemd is needed: the test owns the socket and plays systemd's
//! part. A file descriptor is handed to the child as its stdin, and a shell copies it to 3 and names its own
//! process in LISTEN_PID before it turns into httpdog, which keeps this file free of unsafe code.
#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

use std::{
    fs,
    io::{BufRead, BufReader, Read},
    os::{fd::OwnedFd, unix::net::UnixDatagram},
    path::{Path, PathBuf},
    process::{Child, ChildStderr, Command, Stdio},
};

const BIN: &str = env!("CARGO_BIN_EXE_httpdog");

fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hd-sd-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir(&dir).unwrap();
    dir
}

/// A rule file that alerts on every line it is sent, with the path as the text of the alert
fn rule_file(dir: &Path, listen: &str) -> PathBuf {
    let path = dir.join("rules.yaml");
    let yaml = format!(
        "listen: {listen}\n\
         notifiers: {{ out: {{ type: stderr }} }}\n\
         rules:\n\
         \x20 - id: every-line\n\
         \x20   cooldown: 0s\n\
         \x20   notify: [out]\n\
         \x20   when: {{ field: status, op: eq, value: 404 }}\n\
         \x20   message: \"saw {{request_uri}}\"\n"
    );
    fs::write(&path, yaml).unwrap();
    path
}

struct Httpdog {
    child: Child,
    stderr: BufReader<ChildStderr>,
}

impl Httpdog {
    /// `fds` is LISTEN_FDS. The socket is passed either way, so 0 shows that httpdog leaves it alone
    /// unless it is told the socket is there
    fn start(socket: &UnixDatagram, rules: &Path, fds: u32) -> Httpdog {
        let passed = OwnedFd::from(socket.try_clone().unwrap());
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(r#"exec 3<&0 && LISTEN_PID=$$ LISTEN_FDS="$2" exec "$0" --config "$1""#)
            .arg(BIN)
            .arg(rules)
            .arg(fds.to_string())
            .stdin(Stdio::from(passed))
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = BufReader::new(child.stderr.take().unwrap());
        Httpdog { child, stderr }
    }

    /// The next line of httpdog's log that contains `needle`. Everything read on the way is returned with it
    fn wait_for(&mut self, needle: &str) -> String {
        let mut seen = String::new();
        loop {
            let mut line = String::new();
            let n = self.stderr.read_line(&mut line).unwrap();
            seen.push_str(&line);
            assert!(n > 0, "httpdog exited before logging {needle:?}:\n{seen}");
            if line.contains(needle) {
                return seen;
            }
        }
    }

    /// The way a crash or `systemctl kill -s KILL` ends it: no chance to tidy up
    fn kill(mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }

    fn exit_status(mut self) -> (Option<i32>, String) {
        let mut rest = String::new();
        self.stderr.read_to_string(&mut rest).unwrap();
        (self.child.wait().unwrap().code(), rest)
    }
}

fn log_line(path: &str) -> Vec<u8> {
    format!(r#"<190>Nov 21 00:00:00 nginx: {{"request_uri":"{path}","status":404}}"#).into_bytes()
}

#[test]
fn a_restart_keeps_the_socket_and_the_lines_sent_meanwhile() {
    let dir = test_dir("restart");
    let path = dir.join("s");
    let socket = UnixDatagram::bind(&path).unwrap();
    let rules = rule_file(&dir, &format!("unix:{}", path.display()));

    // The sender connects once and never again, as an nginx worker does
    let nginx = UnixDatagram::unbound().unwrap();
    nginx.connect(&path).unwrap();

    let mut first = Httpdog::start(&socket, &rules, 1);
    let log = first.wait_for("httpdog started");
    assert!(log.contains("(socket held by systemd)"), "{log}");
    nginx.send(&log_line("/before")).unwrap();
    first.wait_for("saw /before");
    first.kill();

    // Nobody is reading now. The line waits in the socket's queue
    nginx.send(&log_line("/while-down")).unwrap();

    let mut second = Httpdog::start(&socket, &rules, 1);
    second.wait_for("saw /while-down");
    nginx.send(&log_line("/after")).unwrap();
    second.wait_for("saw /after");
    second.kill();

    // The socket file is not httpdog's to remove or replace
    assert!(path.exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_passed_socket_that_listen_does_not_name_is_a_config_error() {
    let dir = test_dir("mismatch");
    let path = dir.join("s");
    let socket = UnixDatagram::bind(&path).unwrap();
    let rules = rule_file(&dir, &format!("unix:{}", dir.join("other").display()));

    let (code, log) = Httpdog::start(&socket, &rules, 1).exit_status();
    assert_eq!(code, Some(78), "{log}");
    assert!(
        log.contains("config error: the socket systemd passed is not the one the rule file names")
    );
    assert!(
        log.contains(&format!("it is unix:{}", path.display())),
        "{log}"
    );
    // It did not go and create the socket the rule file names either
    assert!(!dir.join("other").exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_reload_that_changes_listen_is_refused() {
    let dir = test_dir("reload");
    let path = dir.join("s");
    let socket = UnixDatagram::bind(&path).unwrap();
    let listen = format!("unix:{}", path.display());
    let rules = rule_file(&dir, &listen);
    let nginx = UnixDatagram::unbound().unwrap();
    nginx.connect(&path).unwrap();

    let mut httpdog = Httpdog::start(&socket, &rules, 1);
    httpdog.wait_for("httpdog started");
    let reload = |httpdog: &Httpdog| {
        let hup = Command::new("kill")
            .arg("-HUP")
            .arg(httpdog.child.id().to_string())
            .status()
            .unwrap();
        assert!(hup.success());
    };

    // Another listen, and a rule that says "new" in place of "saw": neither may take effect
    let other = format!("unix:{}", dir.join("other").display());
    let changed = fs::read_to_string(&rules)
        .unwrap()
        .replace(&listen, &other)
        .replace("saw ", "new ");
    fs::write(&rules, &changed).unwrap();
    reload(&httpdog);
    let log = httpdog.wait_for("reload failed, keeping the old rules");
    assert!(log.contains(&format!("listen is now {other}")), "{log}");
    assert!(log.contains(&format!("systemd holds the socket {listen}")));
    assert!(!log.contains("rules reloaded"), "{log}");
    nginx.send(&log_line("/old-rules")).unwrap();
    httpdog.wait_for("saw /old-rules");

    // With listen put back, the same rule file is taken
    fs::write(&rules, changed.replace(&other, &listen)).unwrap();
    reload(&httpdog);
    httpdog.wait_for("rules reloaded");
    nginx.send(&log_line("/new-rules")).unwrap();
    httpdog.wait_for("new /new-rules");
    httpdog.kill();
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn without_the_announcement_httpdog_opens_its_own_socket() {
    let dir = test_dir("own");
    let inherited = UnixDatagram::bind(dir.join("inherited")).unwrap();
    let path = dir.join("s");
    let rules = rule_file(&dir, &format!("unix:{}", path.display()));

    let mut httpdog = Httpdog::start(&inherited, &rules, 0);
    let log = httpdog.wait_for("httpdog started");
    assert!(!log.contains("held by systemd"), "{log}");
    UnixDatagram::unbound()
        .unwrap()
        .send_to(&log_line("/own"), &path)
        .unwrap();
    httpdog.wait_for("saw /own");
    httpdog.kill();
    fs::remove_dir_all(dir).unwrap();
}
