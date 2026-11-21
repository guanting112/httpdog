//! Reads a systemd EnvironmentFile, the kind /etc/httpdog/secrets.env is.
//!
//! The service never needs this: systemd reads the file and puts the variables in httpdog's environment.
//! `httpdog -t` does, because it runs from a shell that has none of them, and a check that cannot see the
//! webhook URL and secret can only report them missing.
//!
//! What matters is that a value read here is the value systemd would have passed, so the rules below are
//! systemd's (the parser in its src/basic/env-file.c), not a shell's:
//!
//! - A line is `KEY=value`. Empty lines, lines without `=`, and lines that start with `#` or `;` are skipped.
//! - Whitespace around the key and around the value is dropped.
//! - A value may start with quoted pieces: `'...'` keeps everything inside as it is, and `"..."` does too,
//!   except that a backslash before `"`, `\`, `` ` `` or `$` yields that character. Pieces next to each other
//!   are joined, and a quoted piece may run over several lines.
//! - Once the unquoted part of a value has begun, a quote is an ordinary character: `a "b"` is `a "b"`.
//!   A backslash there yields the character after it, and at the end of a line joins the next line on.
//! - Nothing is expanded: `$HOME` stays `$HOME`.
//! - A later assignment of the same name replaces an earlier one.
//!
//! tests/data/secrets.env holds lines of every kind, and tests/data/secrets.env.expected is what systemd made
//! of that file. A test below checks that this parser makes the same.

/// The variables a file assigns, in the order they first appear. Never fails: like systemd, it takes what it
/// can read and skips the rest.
pub fn parse(text: &str) -> Vec<(String, String)> {
    #[derive(Clone, Copy)]
    enum State {
        PreKey,
        Key,
        PreValue,
        Value,
        ValueEscape,
        SingleQuote,
        DoubleQuote,
        DoubleQuoteEscape,
        Comment,
        CommentEscape,
    }
    use State::*;

    let is_newline = |c: char| c == '\n' || c == '\r';
    let is_space = |c: char| c == ' ' || c == '\t' || is_newline(c);

    let mut vars: Vec<(String, String)> = Vec::new();
    let mut key = String::new();
    let mut value = String::new();
    // Where the whitespace at the end of an unquoted value starts, so it can be cut off again
    let mut trailing: Option<usize> = None;
    let mut state = PreKey;

    let mut push = |key: &mut String, value: &mut String, trailing: &mut Option<usize>| {
        if let Some(at) = trailing.take() {
            value.truncate(at);
        }
        let name = key.trim();
        if is_name(name) {
            let value = std::mem::take(value);
            match vars.iter_mut().find(|(k, _)| k == name) {
                Some(slot) => slot.1 = value,
                None => vars.push((name.to_string(), value)),
            }
        }
        key.clear();
        value.clear();
    };

    for c in text.chars() {
        state = match state {
            PreKey if c == '#' || c == ';' => Comment,
            PreKey if is_space(c) => PreKey,
            PreKey => {
                key.push(c);
                Key
            }
            Key if is_newline(c) => {
                key.clear();
                PreKey
            }
            Key if c == '=' => PreValue,
            Key => {
                key.push(c);
                Key
            }
            PreValue if is_newline(c) => {
                push(&mut key, &mut value, &mut trailing);
                PreKey
            }
            PreValue if c == '\'' => SingleQuote,
            PreValue if c == '"' => DoubleQuote,
            PreValue if c == '\\' => ValueEscape,
            PreValue if is_space(c) => PreValue,
            PreValue => {
                value.push(c);
                Value
            }
            Value if is_newline(c) => {
                push(&mut key, &mut value, &mut trailing);
                PreKey
            }
            Value if c == '\\' => {
                trailing = None;
                ValueEscape
            }
            Value => {
                if is_space(c) {
                    trailing.get_or_insert(value.len());
                } else {
                    trailing = None;
                }
                value.push(c);
                Value
            }
            ValueEscape => {
                // A backslash at the end of a line joins it to the next one
                if !is_newline(c) {
                    value.push(c);
                }
                Value
            }
            SingleQuote if c == '\'' => PreValue,
            SingleQuote => {
                value.push(c);
                SingleQuote
            }
            DoubleQuote if c == '"' => PreValue,
            DoubleQuote if c == '\\' => DoubleQuoteEscape,
            DoubleQuote => {
                value.push(c);
                DoubleQuote
            }
            DoubleQuoteEscape => {
                if matches!(c, '"' | '\\' | '`' | '$') {
                    value.push(c);
                } else if c != '\n' {
                    value.push('\\');
                    value.push(c);
                }
                DoubleQuote
            }
            Comment if c == '\\' => CommentEscape,
            Comment if is_newline(c) => PreKey,
            Comment => Comment,
            CommentEscape => Comment,
        };
    }
    // The last line may end without a newline
    if matches!(
        state,
        PreValue | Value | ValueEscape | SingleQuote | DoubleQuote | DoubleQuoteEscape
    ) {
        push(&mut key, &mut value, &mut trailing);
    }
    vars
}

/// What systemd accepts as the name of an environment variable; an assignment to anything else is dropped
fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str, name: &str) -> Option<String> {
        parse(text)
            .into_iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v)
    }

    #[test]
    fn the_file_the_readme_has_you_write() {
        let text = "HTTPDOG_WEBHOOK_URL=https://example.com/hook?a=1&b=2\n\
                    HTTPDOG_WEBHOOK_SECRET=0123abcd\n";
        assert_eq!(
            parse(text),
            vec![
                (
                    "HTTPDOG_WEBHOOK_URL".to_string(),
                    "https://example.com/hook?a=1&b=2".to_string()
                ),
                ("HTTPDOG_WEBHOOK_SECRET".to_string(), "0123abcd".to_string()),
            ]
        );
    }

    #[test]
    fn quotes_comments_and_whitespace() {
        assert_eq!(value("A='x & y'\n", "A").unwrap(), "x & y");
        assert_eq!(
            value("A=\"x \\\"y\\\" \\n\"\n", "A").unwrap(),
            "x \"y\" \\n"
        );
        assert_eq!(value("  A  =  x y  \n", "A").unwrap(), "x y");
        assert_eq!(value("A=x\\ y\\#\n", "A").unwrap(), "x y#");
        assert_eq!(value("A='x'\"y\"z\n", "A").unwrap(), "xyz");
        assert_eq!(value("A=x 'y' \"z\"\n", "A").unwrap(), "x 'y' \"z\"");
        assert_eq!(value("A=$HOME\n", "A").unwrap(), "$HOME");
        // The last assignment wins, and the last line needs no newline
        assert_eq!(value("A=1\nA=2", "A").unwrap(), "2");
        assert_eq!(value("A=\n", "A").unwrap(), "");
        assert_eq!(value("A=x\r\nB=y\r\n", "B").unwrap(), "y");

        let skipped = "# A=1\n; A=2\nno equals sign\n1A=3\nexport A=4\n\n";
        assert_eq!(parse(skipped), vec![]);
    }

    /// The corpus and what systemd made of it. To make the expected file again, on a machine with systemd:
    ///
    ///   systemd-run --quiet --wait --pipe -p EnvironmentFile=$PWD/tests/data/secrets.env \
    ///     /bin/sh -c 'for n in $(seq 1 40); do eval "v=\${T$n-<unset>}"; printf "T%s=[%s]\n" "$n" "$v"; done' \
    ///     > tests/data/secrets.env.expected
    #[test]
    fn reads_the_corpus_the_way_systemd_does() {
        let vars = parse(include_str!("../tests/data/secrets.env"));
        let mut made = String::new();
        for n in 1..=40 {
            let name = format!("T{n}");
            let v = vars
                .iter()
                .find(|(k, _)| *k == name)
                .map_or("<unset>", |(_, v)| v);
            made.push_str(&format!("{name}=[{v}]\n"));
        }
        assert_eq!(made, include_str!("../tests/data/secrets.env.expected"));
    }
}
