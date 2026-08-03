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
            stdout_file: cmd.stdout_file.clone(),
            append: cmd.append,
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
                    if let Some(last) = history.last() {
                        if let Some(arg) = last.split_whitespace().next_back() {
                            result.push_str(arg);
                        }
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
                    if end > start {
                        if let Ok(n) = std::str::from_utf8(&bytes[start..end])
                            .unwrap()
                            .parse::<usize>()
                        {
                            if n > 0 && n <= history.len() {
                                result.push_str(&history[history.len() - n]);
                                i = end;
                                continue;
                            }
                        }
                    }
                }
                c if c.is_ascii_digit() => {
                    // !n → 第 n 条命令（1-based）
                    let start = i + 1;
                    let mut end = start;
                    while end < bytes.len() && bytes[end].is_ascii_digit() {
                        end += 1;
                    }
                    if let Ok(n) = std::str::from_utf8(&bytes[start..end])
                        .unwrap()
                        .parse::<usize>()
                    {
                        if n > 0 && n <= history.len() {
                            result.push_str(&history[n - 1]);
                            i = end;
                            continue;
                        }
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
            let rest = if idx < p.len() { &p[idx + 1..] } else { &p[idx..] };
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
