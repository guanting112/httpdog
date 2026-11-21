//! Reading a secrets file (a systemd EnvironmentFile), which `httpdog -t` does. Whatever the file contains,
//! reading it must never panic, and what comes out must be usable as environment variables: names systemd
//! would accept, each one once.
#![no_main]

use httpdog::envfile;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let vars = envfile::parse(text);
    for (i, (name, _)) in vars.iter().enumerate() {
        let mut chars = name.chars();
        let first = chars.next().expect("a name is never empty");
        assert!(
            first.is_ascii_alphabetic() || first == '_',
            "not a variable name: {name:?}"
        );
        assert!(
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "not a variable name: {name:?}"
        );
        assert!(
            vars[..i].iter().all(|(earlier, _)| earlier != name),
            "{name:?} appears twice"
        );
    }
});
