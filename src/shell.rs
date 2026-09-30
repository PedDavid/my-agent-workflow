//! POSIX shell quoting, used to build command lines handed to `exec_cmd`.

/// Quote one word so a POSIX shell reads it back verbatim.
pub fn quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:=@%+,".contains(c))
    {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Quote and join a whole argv.
pub fn join<S: AsRef<str>>(args: &[S]) -> String {
    args.iter()
        .map(|a| quote(a.as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_words_are_untouched() {
        assert_eq!(quote("kitty"), "kitty");
        assert_eq!(quote("unix:/run/user/1000/x.sock"), "unix:/run/user/1000/x.sock");
        assert_eq!(quote("A=b"), "A=b");
    }

    #[test]
    fn specials_are_quoted() {
        assert_eq!(quote(""), "''");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's"), "'it'\\''s'");
        assert_eq!(quote("$(rm -rf /)"), "'$(rm -rf /)'");
        assert_eq!(quote("a\nb"), "'a\nb'");
        assert_eq!(quote("\"x\""), "'\"x\"'");
    }

    #[test]
    fn join_works() {
        assert_eq!(join(&["echo", "hi there", "x"]), "echo 'hi there' x");
    }

    #[test]
    fn roundtrip_through_sh() {
        let words = ["a b", "it's", "$HOME", "`x`", "", "\\n", "*", "é"];
        let line = format!("printf '%s|' {}", join(&words));
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(&line)
            .output()
            .unwrap();
        let expect: String = words.iter().map(|w| format!("{w}|")).collect();
        assert_eq!(String::from_utf8_lossy(&out.stdout), expect);
    }
}
