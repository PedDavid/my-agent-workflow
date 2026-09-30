//! Helpers producing Lua expressions for `hyprctl dispatch` under a Lua config.

/// Produce a Lua double-quoted string literal that evaluates to exactly `s`.
pub fn lua_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Three-digit decimal escapes are never ambiguous with a following digit.
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\{:03}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `{ k = "v", ... }` table of string values (keys must be identifiers).
pub fn lua_table(pairs: &[(&str, String)]) -> String {
    if pairs.is_empty() {
        return "{}".to_string();
    }
    let body: Vec<String> = pairs
        .iter()
        .map(|(k, v)| format!("{k} = {}", lua_str(v)))
        .collect();
    format!("{{ {} }}", body.join(", "))
}

pub fn focus_expr(address: &str) -> String {
    format!(
        "hl.dsp.focus({{ window = {} }})",
        lua_str(&format!("address:{address}"))
    )
}

pub fn close_expr(address: &str) -> String {
    format!(
        "hl.dsp.window.close({{ window = {} }})",
        lua_str(&format!("address:{address}"))
    )
}

pub fn exec_expr(cmd: &str, workspace: &str) -> String {
    if workspace.is_empty() {
        format!("hl.dsp.exec_cmd({})", lua_str(cmd))
    } else {
        format!(
            "hl.dsp.exec_cmd({}, {})",
            lua_str(cmd),
            lua_table(&[("workspace", workspace.to_string())])
        )
    }
}

pub const NOOP_EXPR: &str = "hl.dsp.no_op()";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes() {
        assert_eq!(lua_str("abc"), "\"abc\"");
        assert_eq!(lua_str("a\"b"), "\"a\\\"b\"");
        assert_eq!(lua_str("a\\b"), "\"a\\\\b\"");
        assert_eq!(lua_str("a\nb\rc"), "\"a\\nb\\rc\"");
        assert_eq!(lua_str("a\0"), "\"a\\000\"");
        assert_eq!(lua_str("\x01"), "\"\\001\"");
        assert_eq!(lua_str("é"), "\"é\"");
    }

    #[test]
    fn injection_is_neutralised() {
        let s = lua_str("\"); os.execute(\"rm -rf /\"); (\"");
        // every quote inside is escaped, so the literal is a single string
        let inner = &s[1..s.len() - 1];
        let mut prev_backslash = false;
        for c in inner.chars() {
            if c == '"' {
                assert!(prev_backslash);
            }
            prev_backslash = c == '\\' && !prev_backslash;
        }
    }

    #[test]
    fn exprs() {
        assert_eq!(
            focus_expr("0x55d1"),
            "hl.dsp.focus({ window = \"address:0x55d1\" })"
        );
        assert_eq!(
            close_expr("0x55d1"),
            "hl.dsp.window.close({ window = \"address:0x55d1\" })"
        );
        assert_eq!(
            exec_expr("kitty --class drove-ab", "3 silent"),
            "hl.dsp.exec_cmd(\"kitty --class drove-ab\", { workspace = \"3 silent\" })"
        );
        assert_eq!(exec_expr("x", ""), "hl.dsp.exec_cmd(\"x\")");
    }
}
