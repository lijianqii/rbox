//! 展开：变量展开、历史扩展、tilde 展开、通配符展开。

use super::types::*;

// ─── Pipeline 展开入口 ─────────────────────────────────

/// 对 Pipeline 中所有命令的参数做展开（变量 → tilde → glob）。
pub fn expand_pipeline(pipeline: &Pipeline, last_rc: i32) -> Result<Pipeline, String> {
    let mut new_cmds = Vec::with_capacity(pipeline.cmds.len());
    for cmd in &pipeline.cmds {
        let mut new_argv = Vec::with_capacity(cmd.argv.len());
        for arg in &cmd.argv {
            let expanded = expand_vars(arg, last_rc);
            let expanded = expand_tilde(&expanded);
            let globs = expand_glob(&expanded);
            if globs.is_empty() {
                new_argv.push(expanded);
            } else {
                new_argv.extend(globs);
            }
        }
        new_cmds.push(SimpleCmd {
            argv: new_argv,
            stdin_file: cmd.stdin_file.clone(),
            heredoc: cmd.heredoc.clone(),
            stdout_file: cmd.stdout_file.clone(),
            stderr_file: cmd.stderr_file.clone(),
            append: cmd.append,
            append_err: cmd.append_err,
        });
    }
    Ok(Pipeline {
        cmds: new_cmds,
        background: pipeline.background,
    })
}

// ─── 变量展开 ──────────────────────────────────────────

/// 展开 `$VAR`、`${VAR}`、`$?`、`$$`。
pub fn expand_vars(s: &str, last_rc: i32) -> String {
    let mut result = String::new();
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' {
            match chars.peek() {
                Some('{') => {
                    chars.next();
                    let mut name = String::new();
                    while let Some(&nc) = chars.peek() {
                        if nc == '}' {
                            chars.next();
                            break;
                        }
                        name.push(nc);
                        chars.next();
                    }
                    result.push_str(&lookup_var(&name, last_rc));
                }
                Some('?') => {
                    chars.next();
                    result.push_str(&last_rc.to_string());
                }
                Some('$') => {
                    chars.next();
                    result.push_str(&std::process::id().to_string());
                }
                Some(&c2) if c2.is_ascii_alphabetic() || c2 == '_' => {
                    let mut name = String::new();
                    while let Some(&nc) = chars.peek() {
                        if nc.is_ascii_alphanumeric() || nc == '_' {
                            name.push(nc);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    result.push_str(&lookup_var(&name, last_rc));
                }
                _ => {
                    result.push('$');
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}

fn lookup_var(name: &str, last_rc: i32) -> String {
    if name == "?" {
        return last_rc.to_string();
    }
    if name == "$" {
        return std::process::id().to_string();
    }
    std::env::var(name).unwrap_or_default()
}

// ─── 历史扩展 ──────────────────────────────────────────

/// 历史扩展：`!!` → 上一条命令，`!n` → 第 n 条，`!-n` → 倒数第 n 条，`!$` → 上一条最后参数。
pub fn expand_history(line: &str, history: &[String]) -> String {
    if !line.contains('!') || history.is_empty() {
        return line.to_string();
    }

    let mut result = String::new();
    let bytes = line.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'!' && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            match next {
                b'!' => {
                    if let Some(last) = history.last() {
                        result.push_str(last);
                    }
                    i += 2;
                    continue;
                }
                b'$' => {
                    if let Some(last) = history.last()
                        && let Some(arg) = last.split_whitespace().next_back()
                    {
                        result.push_str(arg);
                    }
                    i += 2;
                    continue;
                }
                b'-' => {
                    // !-n → 倒数第 n 条
                    let start = i + 2;
                    let mut end = start;
                    while end < bytes.len() && bytes[end].is_ascii_digit() {
                        end += 1;
                    }
                    if end > start
                        && let Ok(s) = std::str::from_utf8(&bytes[start..end])
                        && let Ok(n) = s.parse::<usize>()
                        && n > 0
                        && n <= history.len()
                    {
                        result.push_str(&history[history.len() - n]);
                        i = end;
                        continue;
                    }
                }
                c if c.is_ascii_digit() => {
                    // !n → 第 n 条命令（1-based）
                    let start = i + 1;
                    let mut end = start;
                    while end < bytes.len() && bytes[end].is_ascii_digit() {
                        end += 1;
                    }
                    if let Ok(s) = std::str::from_utf8(&bytes[start..end])
                        && let Ok(n) = s.parse::<usize>()
                        && n > 0
                        && n <= history.len()
                    {
                        result.push_str(&history[n - 1]);
                        i = end;
                        continue;
                    }
                }
                _ => {}
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }

    result
}

// ─── Tilde 展开 ────────────────────────────────────────

/// `~` → `$HOME`，`~/path` → `$HOME/path`。
pub fn expand_tilde(s: &str) -> String {
    if s.starts_with('~') && (s == "~" || s.starts_with("~/")) {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        if s == "~" {
            return home;
        } else {
            return format!("{}{}", home, &s[1..]);
        }
    }
    s.to_string()
}

// ─── 通配符展开 ────────────────────────────────────────

/// 对含 `*` `?` `[]` 的词项执行 glob 匹配。
pub fn expand_glob(s: &str) -> Vec<String> {
    if !s.contains('*') && !s.contains('?') && !s.contains('[') {
        return Vec::new();
    }

    let dir = std::path::Path::new(s);
    let (search_dir, pattern) = if s.contains('/') {
        let parent = dir.parent().unwrap_or(std::path::Path::new("."));
        let fname = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        (parent.to_path_buf(), fname)
    } else {
        (std::path::PathBuf::from("."), s.to_string())
    };

    let entries = match std::fs::read_dir(&search_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut matches: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') && !pattern.starts_with('.') {
            continue;
        }
        if glob_match(&pattern, &name) {
            if s.contains('/') {
                let full = search_dir.join(&name);
                matches.push(full.to_string_lossy().into_owned());
            } else {
                matches.push(name);
            }
        }
    }
    if !matches.is_empty() {
        matches.sort();
    }
    matches
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob_match_inner(&p, &t)
}

fn glob_match_inner(p: &[char], t: &[char]) -> bool {
    if p.is_empty() {
        return t.is_empty();
    }
    match p[0] {
        '*' => {
            if p.len() == 1 {
                return true;
            }
            for i in 0..=t.len() {
                if glob_match_inner(&p[1..], &t[i..]) {
                    return true;
                }
            }
            false
        }
        '?' => {
            if t.is_empty() {
                return false;
            }
            glob_match_inner(&p[1..], &t[1..])
        }
        '[' => {
            if t.is_empty() {
                return false;
            }
            let mut idx = 1;
            let mut negate = false;
            if idx < p.len() && p[idx] == '!' {
                negate = true;
                idx += 1;
            }
            let mut matched = false;
            while idx < p.len() && p[idx] != ']' {
                if idx + 2 < p.len() && p[idx + 1] == '-' && p[idx + 2] != ']' {
                    if t[0] >= p[idx] && t[0] <= p[idx + 2] {
                        matched = true;
                    }
                    idx += 3;
                } else {
                    if t[0] == p[idx] {
                        matched = true;
                    }
                    idx += 1;
                }
            }
            let rest = if idx < p.len() {
                &p[idx + 1..]
            } else {
                &p[idx..]
            };
            if matched != negate {
                glob_match_inner(rest, &t[1..])
            } else {
                false
            }
        }
        _ => {
            if t.is_empty() || p[0] != t[0] {
                return false;
            }
            glob_match_inner(&p[1..], &t[1..])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── 变量展开 ──────────────────────────────

    #[test]
    fn expand_simple_var() {
        unsafe {
            std::env::set_var("RBOX_TEST_VAR", "hello");
        }
        assert_eq!(expand_vars("$RBOX_TEST_VAR", 0), "hello");
        unsafe {
            std::env::remove_var("RBOX_TEST_VAR");
        }
    }

    #[test]
    fn expand_brace_var() {
        unsafe {
            std::env::set_var("RBOX_TEST_VAR", "world");
        }
        assert_eq!(expand_vars("${RBOX_TEST_VAR}_x", 0), "world_x");
        unsafe {
            std::env::remove_var("RBOX_TEST_VAR");
        }
    }

    #[test]
    fn expand_exit_code() {
        assert_eq!(expand_vars("rc=$?", 42), "rc=42");
    }

    #[test]
    fn expand_pid() {
        let result = expand_vars("pid=$$", 0);
        assert!(result.starts_with("pid="));
        assert!(result.len() > 4);
    }

    #[test]
    fn expand_unset_var() {
        assert_eq!(expand_vars("$RBOX_NONEXIST", 0), "");
    }

    #[test]
    fn expand_literal_dollar() {
        assert_eq!(expand_vars("cost is $5", 0), "cost is $5");
    }

    // ─── 历史扩展 ──────────────────────────────

    #[test]
    fn hist_bang_bang() {
        let history = vec!["echo hello".to_string()];
        assert_eq!(expand_history("!!", &history), "echo hello");
    }

    #[test]
    fn hist_bang_n() {
        let history = vec!["echo a".to_string(), "echo b".to_string()];
        assert_eq!(expand_history("!1", &history), "echo a");
        assert_eq!(expand_history("!2", &history), "echo b");
    }

    #[test]
    fn hist_bang_minus_n() {
        let history = vec!["echo a".to_string(), "echo b".to_string()];
        assert_eq!(expand_history("!-1", &history), "echo b");
        assert_eq!(expand_history("!-2", &history), "echo a");
    }

    #[test]
    fn hist_bang_dollar() {
        let history = vec!["echo aaa bbb ccc".to_string()];
        assert_eq!(expand_history("echo !$", &history), "echo ccc");
    }

    #[test]
    fn hist_no_expansion_when_empty() {
        assert_eq!(expand_history("!!", &[]), "!!");
    }

    #[test]
    fn hist_no_bang() {
        let history = vec!["echo hello".to_string()];
        assert_eq!(expand_history("echo hi", &history), "echo hi");
    }

    // ─── Tilde 展开 ─────────────────────────────

    #[test]
    fn tilde_only() {
        unsafe {
            std::env::set_var("HOME", "/root");
        }
        assert_eq!(expand_tilde("~"), "/root");
        unsafe {
            std::env::remove_var("HOME");
        }
    }

    #[test]
    fn tilde_with_path() {
        unsafe {
            std::env::set_var("HOME", "/root");
        }
        assert_eq!(expand_tilde("~/dir/file"), "/root/dir/file");
        unsafe {
            std::env::remove_var("HOME");
        }
    }

    #[test]
    fn tilde_not_at_start() {
        assert_eq!(expand_tilde("echo ~"), "echo ~");
    }

    // ─── Glob 匹配 ────────────────────────────

    #[test]
    fn glob_star_match_all() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*.txt", "file.txt"));
        assert!(!glob_match("*.txt", "file.rs"));
    }

    #[test]
    fn glob_question_single_char() {
        assert!(glob_match("?", "a"));
        assert!(!glob_match("?", "ab"));
        assert!(glob_match("a?c", "abc"));
    }

    #[test]
    fn glob_bracket_set() {
        assert!(glob_match("[abc]", "a"));
        assert!(glob_match("[abc]", "b"));
        assert!(!glob_match("[abc]", "d"));
    }

    #[test]
    fn glob_bracket_range() {
        assert!(glob_match("[0-9]", "5"));
        assert!(!glob_match("[0-9]", "a"));
        assert!(glob_match("[a-z]", "x"));
    }

    #[test]
    fn glob_bracket_negate() {
        assert!(!glob_match("[!abc]", "a"));
        assert!(glob_match("[!abc]", "d"));
    }

    #[test]
    fn glob_complex() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(glob_match("file[0-9]?", "file5a"));
        assert!(!glob_match("file[0-9]?", "fileab"));
    }
}
