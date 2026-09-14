//! 展开：变量展开、历史扩展、tilde 展开、通配符展开。

use super::types::*;
use super::types::{NO_SPLIT_ESCAPE, SPLIT_ESCAPE};
use crate::applets::glob::glob_match;

// ─── Pipeline 展开入口 ─────────────────────────────────

/// 对 Pipeline 中所有命令的参数做展开（变量 → tilde → glob）。
pub fn expand_pipeline(pipeline: &Pipeline, last_rc: i32) -> Result<Pipeline, String> {
    let mut new_cmds = Vec::with_capacity(pipeline.cmds.len());
    for cmd in &pipeline.cmds {
        let mut new_argv = Vec::with_capacity(cmd.argv.len());
        for arg in &cmd.argv {
            new_argv.extend(expand_word(arg, last_rc));
        }
        // 重定向目标路径也做展开（`> $file`）
        let expand_path = |p: &Option<String>| -> Option<String> {
            p.as_ref().map(|path| {
                expand_word(path, last_rc)
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| path.clone())
            })
        };
        new_cmds.push(SimpleCmd {
            argv: new_argv,
            stdin_file: expand_path(&cmd.stdin_file),
            heredoc: cmd.heredoc.clone(),
            stdout_file: expand_path(&cmd.stdout_file),
            stderr_file: expand_path(&cmd.stderr_file),
            append: cmd.append,
            append_err: cmd.append_err,
            dup_fds: cmd.dup_fds.clone(),
            env: cmd.env.clone(),
            close_fds: cmd.close_fds.clone(),
            here_string: cmd
                .here_string
                .as_ref()
                .map(|h| expand_word(h, last_rc).join(" ")),
            stderr_to_pipe: cmd.stderr_to_pipe,
            rw_file: expand_path(&cmd.rw_file),
            force: cmd.force,
            fd_redirects: cmd
                .fd_redirects
                .iter()
                .map(|r| crate::applets::core::shell::types::FdRedirect {
                    fd: r.fd,
                    path: expand_word(&r.path, last_rc)
                        .into_iter()
                        .next()
                        .unwrap_or_else(|| r.path.clone()),
                    append: r.append,
                    input: r.input,
                })
                .collect(),
        });
    }
    Ok(Pipeline {
        cmds: new_cmds,
        background: pipeline.background,
    })
}

/// 展开单个词：花括号 → 变量 → tilde → glob → 词分割。
/// 供 expand_pipeline 与复合命令（for 的词表）共用。
pub fn expand_word(arg: &str, last_rc: i32) -> Vec<String> {
    let mut out = Vec::new();
    if super::options::braceexpand() {
        for w in expand_braces(arg) {
            out.extend(expand_word_single(&w, last_rc));
        }
    } else {
        out.extend(expand_word_single(arg, last_rc));
    }
    out
}

/// 单次展开（花括号展开后）。
fn expand_word_single(arg: &str, last_rc: i32) -> Vec<String> {
    // `"$@"` 特例：展开为多个位置参数（无参数时产生零个词）
    let quoted_at = format!("{}$@", NO_SPLIT_ESCAPE);
    if arg == quoted_at {
        return super::params::all();
    }
    // 是否含未加引号的展开（决定是否做词分割）
    let has_unquoted_expansion = arg.contains(SPLIT_ESCAPE);
    let expanded = expand_vars(arg, last_rc);
    let expanded = expand_tilde(&expanded);
    if !super::options::noglob() {
        let globs = expand_glob(&expanded);
        if !globs.is_empty() {
            return globs;
        }
    }
    if has_unquoted_expansion {
        // POSIX 词分割：仅未加引号的展开区间按 IFS 拆分
        let ifs = std::env::var("IFS").unwrap_or_else(|_| " \t\n".to_string());
        let fields = split_marked(&expanded, &ifs);
        if fields.len() > 1 {
            return fields;
        }
    }
    vec![unescape_glob(&expanded)]
}

/// 去除词分割元数据（递归展开时由外层决定是否拆分）。
fn strip_split(s: &str) -> String {
    s.chars()
        .filter(|c| *c != NO_SPLIT_ESCAPE && *c != SPLIT_ESCAPE)
        .collect()
}

/// 标记感知的 IFS 词分割：仅 `SPLIT_ESCAPE` 区间（未加引号展开）参与拆分。
fn split_marked(s: &str, ifs: &str) -> Vec<String> {
    let mut fields: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut splittable = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            SPLIT_ESCAPE => {
                splittable = !splittable;
                continue;
            }
            NO_SPLIT_ESCAPE => continue,
            GLOB_ESCAPE => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
                continue;
            }
            _ => {}
        }
        if splittable && ifs.contains(c) {
            fields.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    fields.push(cur);
    fields.into_iter().filter(|f| !f.is_empty()).collect()
}

/// 展开变量并去除词分割元数据（赋值/heredoc 等不做词分割的场景）。
pub fn expand_vars_clean(s: &str, last_rc: i32) -> String {
    unescape_glob(&expand_vars(s, last_rc))
}

/// 花括号展开：`{a,b}`、`{1..5}`、`{a..e}`（引号内不展开）。
fn expand_braces(word: &str) -> Vec<String> {
    let chars: Vec<char> = word.chars().collect();
    // 找第一个未保护且含 `,` 或 `..` 的 `{...}`
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == GLOB_ESCAPE {
            i += 2;
            continue;
        }
        if chars[i] == '{' {
            // 找匹配的 `}`
            let mut depth = 1;
            let mut j = i + 1;
            let mut parts: Vec<String> = Vec::new();
            let mut cur = String::new();
            while j < chars.len() && depth > 0 {
                let c = chars[j];
                if c == GLOB_ESCAPE {
                    cur.push(c);
                    if j + 1 < chars.len() {
                        cur.push(chars[j + 1]);
                    }
                    j += 2;
                    continue;
                }
                match c {
                    '{' => {
                        depth += 1;
                        cur.push(c);
                    }
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            parts.push(cur.clone());
                            break;
                        }
                        cur.push(c);
                    }
                    ',' if depth == 1 => {
                        parts.push(std::mem::take(&mut cur));
                    }
                    _ => cur.push(c),
                }
                j += 1;
            }
            if depth == 0 {
                // 有逗号或多个部分 -> 展开
                let expanded_parts: Vec<String> = if parts.len() > 1 {
                    parts
                } else {
                    match expand_range(&parts[0]) {
                        Some(r) => r,
                        None => {
                            i += 1;
                            continue;
                        }
                    }
                };
                let before: String = chars[..i].iter().collect();
                let after: String = chars[j + 1..].iter().collect();
                let mut out = Vec::new();
                for p in expanded_parts {
                    out.extend(expand_braces(&format!("{}{}{}", before, p, after)));
                }
                return out;
            }
        }
        i += 1;
    }
    vec![word.to_string()]
}

/// `{1..5}` / `{a..e}` 范围展开。
fn expand_range(spec: &str) -> Option<Vec<String>> {
    let (a, b) = spec.split_once("..")?;
    if let (Ok(x), Ok(y)) = (a.parse::<i64>(), b.parse::<i64>()) {
        let mut out = Vec::new();
        let step = if x <= y { 1 } else { -1 };
        let mut v = x;
        loop {
            out.push(v.to_string());
            if v == y {
                break;
            }
            v += step;
            if out.len() > 100000 {
                break;
            }
        }
        return Some(out);
    }
    let mut ac = a.chars();
    let mut bc = b.chars();
    let (x, y) = (ac.next()?, bc.next()?);
    if ac.next().is_some() || bc.next().is_some() {
        return None;
    }
    let mut out = Vec::new();
    let step: i32 = if x <= y { 1 } else { -1 };
    let mut v = x as i32;
    loop {
        out.push(char::from_u32(v as u32)?.to_string());
        if v == y as i32 {
            break;
        }
        v += step;
        if out.len() > 100000 {
            break;
        }
    }
    Some(out)
}

// ─── 变量展开 ──────────────────────────────────────────

/// 展开 `$VAR`、`${VAR}`、`$?`、`$$`。
pub fn expand_vars(s: &str, last_rc: i32) -> String {
    let mut result = String::new();
    let mut chars = s.chars().peekable();
    let mut split_next = false;

    while let Some(c) = chars.next() {
        if c == NO_SPLIT_ESCAPE {
            split_next = false;
            continue;
        }
        if c == SPLIT_ESCAPE {
            split_next = true;
            continue;
        }
        if c == GLOB_ESCAPE {
            // 保护标记：`$` 前的标记表示字面美元（单引号/转义），
            // 其余标记保留给 expand_glob / unescape_glob
            if chars.peek() == Some(&'$') {
                chars.next();
                result.push('$');
            } else {
                result.push(c);
            }
            continue;
        }
        if c == '$' {
            let start = result.len();
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
                    result.push_str(&expand_braced(&name, last_rc));
                }
                Some('?') => {
                    chars.next();
                    result.push_str(&last_rc.to_string());
                }
                Some('$') => {
                    chars.next();
                    result.push_str(&std::process::id().to_string());
                }
                Some('!') => {
                    chars.next();
                    result.push_str(&super::params::last_bg().to_string());
                }
                Some('-') => {
                    chars.next();
                    result.push_str(&super::options::option_string());
                }
                Some('#') => {
                    chars.next();
                    result.push_str(&super::params::count().to_string());
                }
                Some('@') | Some('*') => {
                    chars.next();
                    result.push_str(&super::params::all().join(" "));
                }
                Some('(') => {
                    // `$((...))` 算术展开（克隆迭代器探测第二个 '('）
                    let mut probe = chars.clone();
                    probe.next(); // consume '('
                    if probe.peek() == Some(&'(') {
                        chars.next();
                        chars.next();
                        let mut expr = String::new();
                        let mut depth = 1usize;
                        while let Some(ch) = chars.next() {
                            match ch {
                                '(' => {
                                    depth += 1;
                                    expr.push(ch);
                                }
                                ')' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        if chars.peek() == Some(&')') {
                                            chars.next();
                                        }
                                        break;
                                    }
                                    expr.push(ch);
                                }
                                _ => expr.push(ch),
                            }
                        }
                        result.push_str(&eval_arith(&expr).unwrap_or(0).to_string());
                    } else {
                        result.push('$');
                    }
                }
                Some(&c2) if c2.is_ascii_digit() => {
                    chars.next();
                    result.push_str(&lookup_var(&c2.to_string(), last_rc));
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
            if split_next && result.len() > start {
                // 未加引号的展开：包裹分割标记，供词分割阶段识别区间
                let val: String = result[start..].to_string();
                result.truncate(start);
                result.push(SPLIT_ESCAPE);
                result.push_str(&val);
                result.push(SPLIT_ESCAPE);
            }
            split_next = false;
        } else {
            result.push(c);
        }
    }
    result
}

/// `${...}` 参数展开：支持 `:-` `:=` `:?` `:+`、`${#var}`、`#`/`##`/`%`/`%%` 模式删除、`/`/`//` 替换。
fn expand_braced(spec: &str, last_rc: i32) -> String {
    // 去除引号保护标记（如 `${p:1:3}` 中 `}` 前的标记）
    let spec_clean = spec.replace(GLOB_ESCAPE, "");
    let spec = spec_clean.as_str();
    if spec.starts_with('!') {
        // ash：bad substitution
        eprintln!("shell: syntax error: bad substitution");
        return String::new();
    }
    if let Some(name) = spec.strip_prefix('#') {
        return lookup_var(name, last_rc).chars().count().to_string();
    }
    let valid_name = |n: &str| {
        !n.is_empty()
            && n.chars().all(|c| {
                c.is_ascii_alphanumeric()
                    || c == '_'
                    || matches!(c, '@' | '*' | '?' | '$' | '!' | '#')
            })
    };
    // ${var:offset[:length]} 子串（在 :- := :? :+ 之后检查）
    if let Some(pos) = spec.find(':')
        && pos > 0
        && !matches!(
            spec.as_bytes().get(pos + 1),
            Some(b'-') | Some(b'=') | Some(b'?') | Some(b'+')
        )
    {
        let name = &spec[..pos];
        if valid_name(name) {
            let rest = &spec[pos + 1..];
            let (off_s, len_s) = match rest.split_once(':') {
                Some((a, b)) => (a, Some(b)),
                None => (rest, None),
            };
            if let Ok(off) = off_s.trim().parse::<i64>() {
                let val = lookup_var(name, last_rc);
                let chars: Vec<char> = val.chars().collect();
                let n = chars.len() as i64;
                let start = if off < 0 {
                    (n + off).max(0)
                } else {
                    off.min(n)
                } as usize;
                let end = match len_s.and_then(|l| l.trim().parse::<i64>().ok()) {
                    Some(l) if l >= 0 => (start as i64 + l).min(n) as usize,
                    Some(l) => (n + l).max(start as i64) as usize,
                    None => n as usize,
                };
                return chars[start..end.min(chars.len())].iter().collect();
            }
        }
    }
    for op in [":-", ":=", ":?", ":+", "##", "#", "%%", "%", "/"] {
        if let Some(pos) = spec.find(op) {
            let name = &spec[..pos];
            if !valid_name(name) {
                continue;
            }
            let arg = &spec[pos + op.len()..];
            let val = lookup_var(name, last_rc);
            match op {
                ":-" => {
                    return if val.is_empty() {
                        expand_vars(&strip_split(arg), last_rc)
                    } else {
                        val
                    };
                }
                ":=" => {
                    return if val.is_empty() {
                        let v = expand_vars(&strip_split(arg), last_rc);
                        // SAFETY: shell 单线程
                        unsafe {
                            std::env::set_var(name, &v);
                        }
                        v
                    } else {
                        val
                    };
                }
                ":?" => {
                    if val.is_empty() {
                        eprintln!(
                            "shell: {}: {}",
                            name,
                            if arg.is_empty() {
                                "parameter null or not set"
                            } else {
                                arg
                            }
                        );
                        super::options::mark_nounset_violation();
                    }
                    return val;
                }
                ":+" => {
                    return if val.is_empty() {
                        String::new()
                    } else {
                        expand_vars(&strip_split(arg), last_rc)
                    };
                }
                "##" => return remove_prefix(&val, &unescape_glob(arg), true),
                "#" => return remove_prefix(&val, &unescape_glob(arg), false),
                "%%" => return remove_suffix(&val, &unescape_glob(arg), true),
                "%" => return remove_suffix(&val, &unescape_glob(arg), false),
                "/" => {
                    let (pat, rep, all) = if let Some(rest) = arg.strip_prefix('/') {
                        let (p, r) = rest.split_once('/').unwrap_or((rest, ""));
                        (p, r, true)
                    } else {
                        let (p, r) = arg.split_once('/').unwrap_or((arg, ""));
                        (p, r, false)
                    };
                    let rep = expand_vars(&strip_split(rep), last_rc);
                    return if all {
                        val.replace(pat, &rep)
                    } else {
                        val.replacen(pat, &rep, 1)
                    };
                }
                _ => {}
            }
        }
    }
    lookup_var(spec, last_rc)
}

/// 最短/最长前缀删除（pattern 用 glob 匹配）。
fn remove_prefix(val: &str, pat: &str, longest: bool) -> String {
    let mut idxs: Vec<usize> = val.char_indices().map(|(i, _)| i).collect();
    idxs.push(val.len());
    let ordered: Vec<usize> = if longest {
        idxs.into_iter().rev().collect()
    } else {
        idxs
    };
    for i in ordered {
        if crate::applets::glob::glob_match(pat, &val[..i]) {
            return val[i..].to_string();
        }
    }
    val.to_string()
}

/// 最短/最长后缀删除（pattern 用 glob 匹配）。
fn remove_suffix(val: &str, pat: &str, longest: bool) -> String {
    let mut idxs: Vec<usize> = val.char_indices().map(|(i, _)| i).collect();
    idxs.push(val.len());
    let ordered: Vec<usize> = if longest {
        idxs.into_iter().collect()
    } else {
        idxs.into_iter().rev().collect()
    };
    for i in ordered {
        if crate::applets::glob::glob_match(pat, &val[i..]) {
            return val[..i].to_string();
        }
    }
    val.to_string()
}

fn lookup_var(name: &str, last_rc: i32) -> String {
    match name {
        "?" => return last_rc.to_string(),
        "$" => return std::process::id().to_string(),
        "!" => return super::params::last_bg().to_string(),
        "-" => return super::options::option_string(),
        "RANDOM" => return (random_u32() % 32768).to_string(),
        "#" => return super::params::count().to_string(),
        "@" | "*" => return super::params::all().join(" "),
        "0" => return super::params::get0(),
        _ => {}
    }
    if let Ok(n) = name.parse::<usize>() {
        return match super::params::get(n.saturating_sub(1)) {
            Some(v) => v,
            None => {
                if super::options::nounset() {
                    eprintln!("shell: ${}: unbound variable", name);
                    super::options::mark_nounset_violation();
                }
                String::new()
            }
        };
    }
    match std::env::var(name) {
        Ok(v) => v,
        Err(_) => {
            if super::options::nounset() {
                eprintln!("shell: {}: unbound variable", name);
                super::options::mark_nounset_violation();
            }
            String::new()
        }
    }
}

// ─── 算术展开 $((...)) ─────────────────────────────────

/// `$RANDOM`：简单 LCG（种子来自 pid+时间）。
fn random_u32() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static STATE: AtomicU32 = AtomicU32::new(0);
    let mut x = STATE.load(Ordering::Relaxed);
    if x == 0 {
        x = std::process::id().wrapping_mul(2654435761)
            ^ std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(12345);
    }
    x = x.wrapping_mul(1664525).wrapping_add(1013904223);
    STATE.store(x, Ordering::Relaxed);
    x
}

/// 求值算术表达式（整数：+ - * / % 与括号，标识符取环境变量）。
/// 非法表达式或除零返回 None（调用方按 0 处理并告警）。
pub(crate) fn eval_arith(expr: &str) -> Option<i64> {
    let mut p = ArithParser {
        s: expr.as_bytes(),
        pos: 0,
    };
    let v = p.expr()?;
    p.ws();
    if p.pos != p.s.len() {
        return None;
    }
    Some(v)
}

/// 读取整数型变量值（未设置/非数字按 0）。
fn var_i64(name: &str) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(0)
}

/// 写入整数型变量值。
fn set_var_i64(name: &str, val: i64) {
    // SAFETY: shell 单线程
    unsafe {
        std::env::set_var(name, val.to_string());
    }
}

struct ArithParser<'a> {
    s: &'a [u8],
    pos: usize,
}

impl ArithParser<'_> {
    fn ws(&mut self) {
        while self.pos < self.s.len() {
            let c = self.s[self.pos];
            if c.is_ascii_whitespace() || c == GLOB_ESCAPE as u8 {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    /// expr := assignment (',' assignment)*（返回最后一个）
    fn expr(&mut self) -> Option<i64> {
        let mut v = self.assignment()?;
        loop {
            self.ws();
            if self.peek() == Some(b',') {
                self.pos += 1;
                v = self.assignment()?;
            } else {
                break;
            }
        }
        Some(v)
    }

    /// 读取标识符。
    fn read_ident(&mut self) -> Option<String> {
        self.ws();
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        if start == self.pos {
            return None;
        }
        std::str::from_utf8(&self.s[start..self.pos])
            .ok()
            .map(str::to_string)
    }

    /// assignment := IDENT ('='|'+='|'-='|'*='|'/'|'%=') assignment
    fn assignment(&mut self) -> Option<i64> {
        let save = self.pos;
        self.ws();
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let name_end = self.pos;
        self.ws();
        let op = if self.peek() == Some(b'=') && self.s.get(self.pos + 1) != Some(&b'=') {
            Some('=')
        } else if matches!(self.peek(), Some(b'+' | b'-' | b'*' | b'/' | b'%'))
            && self.s.get(self.pos + 1) == Some(&b'=')
        {
            self.s.get(self.pos).map(|b| *b as char)
        } else {
            None
        };
        if name_end > start
            && let Some(op) = op
            && let Ok(name) = std::str::from_utf8(&self.s[start..name_end])
        {
            let name = name.to_string();
            self.pos += if op == '=' { 1 } else { 2 };
            let rhs = self.assignment()?;
            let cur = std::env::var(&name)
                .ok()
                .and_then(|v| v.trim().parse::<i64>().ok())
                .unwrap_or(0);
            let val = match op {
                '=' => rhs,
                '+' => cur.checked_add(rhs)?,
                '-' => cur.checked_sub(rhs)?,
                '*' => cur.checked_mul(rhs)?,
                '/' => {
                    if rhs == 0 {
                        return None;
                    }
                    cur / rhs
                }
                '%' => {
                    if rhs == 0 {
                        return None;
                    }
                    cur % rhs
                }
                _ => rhs,
            };
            // SAFETY: shell 单线程
            unsafe {
                std::env::set_var(&name, val.to_string());
            }
            return Some(val);
        }
        self.pos = save;
        self.ternary()
    }

    /// ternary := logical_or ('?' ternary ':' ternary)?
    fn ternary(&mut self) -> Option<i64> {
        let cond = self.logical_or()?;
        self.ws();
        if self.peek() == Some(b'?') {
            self.pos += 1;
            let a = self.ternary()?;
            self.ws();
            if self.peek() != Some(b':') {
                return None;
            }
            self.pos += 1;
            let b = self.ternary()?;
            return Some(if cond != 0 { a } else { b });
        }
        Some(cond)
    }

    /// bit_or := bit_xor ('|' bit_xor)*（不吞 '||'）
    fn bit_or(&mut self) -> Option<i64> {
        let mut v = self.bit_xor()?;
        loop {
            self.ws();
            if self.peek() == Some(b'|') && self.s.get(self.pos + 1) != Some(&b'|') {
                self.pos += 1;
                v |= self.bit_xor()?;
            } else {
                break;
            }
        }
        Some(v)
    }

    /// bit_xor := bit_and ('^' bit_and)*
    fn bit_xor(&mut self) -> Option<i64> {
        let mut v = self.bit_and()?;
        loop {
            self.ws();
            if self.peek() == Some(b'^') {
                self.pos += 1;
                v ^= self.bit_and()?;
            } else {
                break;
            }
        }
        Some(v)
    }

    /// bit_and := cmp ('&' cmp)*（不吞 '&&'）
    fn bit_and(&mut self) -> Option<i64> {
        let mut v = self.cmp()?;
        loop {
            self.ws();
            if self.peek() == Some(b'&') && self.s.get(self.pos + 1) != Some(&b'&') {
                self.pos += 1;
                v &= self.cmp()?;
            } else {
                break;
            }
        }
        Some(v)
    }

    /// logical_or := logical_and ('||' logical_and)*
    fn logical_or(&mut self) -> Option<i64> {
        let mut v = self.logical_and()?;
        loop {
            self.ws();
            if self.s.get(self.pos..self.pos + 2) == Some(b"||") {
                self.pos += 2;
                let r = self.logical_and()?;
                v = ((v != 0) || (r != 0)) as i64;
            } else {
                break;
            }
        }
        Some(v)
    }

    /// logical_and := bit_or ('&&' bit_or)*
    fn logical_and(&mut self) -> Option<i64> {
        let mut v = self.bit_or()?;
        loop {
            self.ws();
            if self.s.get(self.pos..self.pos + 2) == Some(b"&&") {
                self.pos += 2;
                let r = self.bit_or()?;
                v = ((v != 0) && (r != 0)) as i64;
            } else {
                break;
            }
        }
        Some(v)
    }

    /// cmp := add (('=='|'!='|'<='|'>='|'<'|'>') add)*
    fn cmp(&mut self) -> Option<i64> {
        let mut v = self.add()?;
        loop {
            self.ws();
            let (op, len) = if self.s.get(self.pos..self.pos + 2) == Some(b"==") {
                ("==", 2)
            } else if self.s.get(self.pos..self.pos + 2) == Some(b"!=") {
                ("!=", 2)
            } else if self.s.get(self.pos..self.pos + 2) == Some(b"<=") {
                ("<=", 2)
            } else if self.s.get(self.pos..self.pos + 2) == Some(b">=") {
                (">=", 2)
            } else if self.peek() == Some(b'<') {
                ("<", 1)
            } else if self.peek() == Some(b'>') {
                (">", 1)
            } else {
                break;
            };
            self.pos += len;
            let r = self.add()?;
            v = match op {
                "==" => (v == r) as i64,
                "!=" => (v != r) as i64,
                "<=" => (v <= r) as i64,
                ">=" => (v >= r) as i64,
                "<" => (v < r) as i64,
                ">" => (v > r) as i64,
                _ => 0,
            };
        }
        Some(v)
    }

    /// add := term (('+'|'-') term)*
    fn add(&mut self) -> Option<i64> {
        let mut v = self.term()?;
        loop {
            self.ws();
            match self.peek() {
                Some(b'+') => {
                    self.pos += 1;
                    v = v.checked_add(self.term()?)?;
                }
                Some(b'-') => {
                    self.pos += 1;
                    v = v.checked_sub(self.term()?)?;
                }
                _ => break,
            }
        }
        Some(v)
    }

    /// term := power (('*'|'/'|'%') power)*
    fn term(&mut self) -> Option<i64> {
        let mut v = self.power()?;
        loop {
            self.ws();
            match self.peek() {
                Some(b'*') if self.s.get(self.pos + 1) != Some(&b'*') => {
                    self.pos += 1;
                    v = v.checked_mul(self.power()?)?;
                }
                Some(b'/') => {
                    self.pos += 1;
                    let d = self.power()?;
                    if d == 0 {
                        return None;
                    }
                    v = v.checked_div(d)?;
                }
                Some(b'%') => {
                    self.pos += 1;
                    let d = self.power()?;
                    if d == 0 {
                        return None;
                    }
                    v = v.checked_rem(d)?;
                }
                _ => break,
            }
        }
        Some(v)
    }

    /// power := factor ('**' power)?（右结合；一元运算符作用于整个幂）
    fn power(&mut self) -> Option<i64> {
        let base = self.factor()?;
        self.ws();
        if self.s.get(self.pos..self.pos + 2) == Some(b"**") {
            self.pos += 2;
            let exp = self.power()?;
            if !(0..=u32::MAX as i64).contains(&exp) {
                return None;
            }
            return base.checked_pow(exp as u32);
        }
        Some(base)
    }

    /// factor := ('+'|'-'|'!')? power | primary
    fn factor(&mut self) -> Option<i64> {
        self.ws();
        match self.peek() {
            Some(b'-') if self.s.get(self.pos + 1) != Some(&b'-') => {
                self.pos += 1;
                Some(-self.power()?)
            }
            Some(b'+') if self.s.get(self.pos + 1) != Some(&b'+') => {
                self.pos += 1;
                self.power()
            }
            Some(b'!') if self.s.get(self.pos + 1) != Some(&b'=') => {
                self.pos += 1;
                Some((self.power()? == 0) as i64)
            }
            Some(b'~') => {
                self.pos += 1;
                Some(!self.power()?)
            }
            Some(b'+') if self.s.get(self.pos + 1) == Some(&b'+') => {
                self.pos += 2;
                let name = self.read_ident()?;
                let val = var_i64(&name) + 1;
                set_var_i64(&name, val);
                Some(val)
            }
            Some(b'-') if self.s.get(self.pos + 1) == Some(&b'-') => {
                self.pos += 2;
                let name = self.read_ident()?;
                let val = var_i64(&name) - 1;
                set_var_i64(&name, val);
                Some(val)
            }
            _ => self.primary(),
        }
    }

    /// primary := NUMBER | IDENT | '(' expr ')' | '$' IDENT
    fn primary(&mut self) -> Option<i64> {
        self.ws();
        if self.peek() == Some(b'(') {
            self.pos += 1;
            let v = self.expr()?;
            self.ws();
            if self.peek() != Some(b')') {
                return None;
            }
            self.pos += 1;
            return Some(v);
        }
        if self.peek() == Some(b'$') {
            self.pos += 1;
        }
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        if start == self.pos {
            return None;
        }
        let tok = std::str::from_utf8(&self.s[start..self.pos]).ok()?;
        if let Ok(n) = tok.parse::<i64>() {
            return Some(n);
        }
        // 标识符：环境变量值按整数解析，未设置/非数字按 0
        let val = var_i64(tok);
        // 后缀 ++/--
        if self.s.get(self.pos..self.pos + 2) == Some(b"++") {
            self.pos += 2;
            set_var_i64(tok, val + 1);
            return Some(val);
        }
        if self.s.get(self.pos..self.pos + 2) == Some(b"--") {
            self.pos += 2;
            set_var_i64(tok, val - 1);
            return Some(val);
        }
        Some(val)
    }
}

// ─── 历史扩展 ──────────────────────────────────────────

/// 历史扩展：`!!` → 上一条命令，`!n` → 第 n 条，`!-n` → 倒数第 n 条，`!$` → 上一条最后参数。
/// 单引号内的 `!` 不展开（字面保留）；双引号内与引号外展开。
pub fn expand_history(line: &str, history: &[String]) -> String {
    if !line.contains('!') || history.is_empty() {
        return line.to_string();
    }

    let mut result = String::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_squote = false;

    while i < bytes.len() {
        let ch = line[i..].chars().next().unwrap();
        if ch == '\'' {
            in_squote = !in_squote;
            result.push(ch);
            i += ch.len_utf8();
            continue;
        }
        if !in_squote && ch == '!' && i + 1 < bytes.len() {
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
        result.push(ch);
        i += ch.len_utf8();
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

/// 词中是否含有未受保护的 glob 元字符（`*` `?` `[`）。
/// `GLOB_ESCAPE` 标记后的字符视为字面（引号/转义保护）。
fn has_active_glob(s: &str) -> bool {
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == NO_SPLIT_ESCAPE || c == SPLIT_ESCAPE {
            // 引号/分割元数据标记：跳过，展开照常
            continue;
        }
        if c == GLOB_ESCAPE {
            chars.next(); // 跳过被保护的字符
            continue;
        }
        if matches!(c, '*' | '?' | '[') {
            return true;
        }
    }
    false
}

/// 移除 glob 保护标记，使引号/转义保护的元字符变为字面字符。
pub fn unescape_glob(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == NO_SPLIT_ESCAPE || c == SPLIT_ESCAPE {
            // 引号/分割元数据标记：跳过，展开照常
            continue;
        }
        if c == GLOB_ESCAPE {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// 对含 `*` `?` `[]` 的词项执行 glob 匹配。
/// 引号/反斜杠保护的元字符（带 `GLOB_ESCAPE` 标记）按字面匹配；
/// 无活跃通配符时返回空 Vec（调用方保留原词并 unescape）。
pub fn expand_glob(s: &str) -> Vec<String> {
    let cleaned = strip_split(s);
    let s = cleaned.as_str();
    if !has_active_glob(s) {
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
        // 孤立 $ 与非法字符保留字面
        assert_eq!(expand_vars("cost is $", 0), "cost is $");
        assert_eq!(expand_vars("cost is $%", 0), "cost is $%");
    }

    #[test]
    fn expand_positional_params() {
        super::super::params::set(vec!["a".into(), "b c".into()]);
        assert_eq!(expand_vars("$1/$2", 0), "a/b c");
        assert_eq!(expand_vars("$#", 0), "2");
        assert_eq!(expand_vars("$@", 0), "a b c");
        assert_eq!(expand_vars("${1}-${2}", 0), "a-b c");
        assert_eq!(expand_vars("$9", 0), "");
        super::super::params::set(Vec::new());
    }

    #[test]
    fn protected_dollar_is_literal() {
        // 单引号/转义产生的保护标记：$ 不展开
        let escaped = format!("{}$VAR", GLOB_ESCAPE);
        assert_eq!(expand_vars(&escaped, 0), "$VAR");
        // 其他保护标记保留（给 glob 用）
        let star = format!("{}*", GLOB_ESCAPE);
        assert_eq!(expand_vars(&star, 0), star);
    }

    #[test]
    fn arithmetic_eval() {
        assert_eq!(eval_arith("1+2*3"), Some(7));
        assert_eq!(eval_arith("(1+2)*3"), Some(9));
        assert_eq!(eval_arith("10/3"), Some(3));
        assert_eq!(eval_arith("10%3"), Some(1));
        assert_eq!(eval_arith("-2+5"), Some(3));
        assert_eq!(eval_arith("1/0"), None);
        assert_eq!(eval_arith("1+"), None);
        assert_eq!(expand_vars("$((1+2*3))", 0), "7");
        assert_eq!(expand_vars("n=$((2+3))", 0), "n=5");
        // 双引号内 * 带 glob 保护标记，算术求值需忽略标记
        let quoted = format!("$((2{}*3))", GLOB_ESCAPE);
        assert_eq!(expand_vars(&quoted, 0), "6");
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

    #[test]
    fn hist_bang_in_single_quotes_not_expanded() {
        let history = vec!["echo hello".to_string()];
        // 单引号内 !! 字面保留
        assert_eq!(expand_history("echo '!!'", &history), "echo '!!'");
    }

    #[test]
    fn hist_bang_outside_single_quotes_expands() {
        let history = vec!["echo hello".to_string()];
        assert_eq!(
            expand_history("echo 'a' !!", &history),
            "echo 'a' echo hello"
        );
    }

    #[test]
    fn hist_utf8_with_bang_preserved() {
        let history = vec!["echo 你好".to_string()];
        // 非 ASCII 字符应原样保留，不被按字节拆坏
        assert_eq!(expand_history("echo 你好!", &history), "echo 你好!");
        assert_eq!(
            expand_history("echo 你好!!", &history),
            "echo 你好echo 你好"
        );
    }

    // ─── Tilde 展开（合并为单测试：多个测试并行 set/remove 共享的 HOME 会互相干扰）───

    #[test]
    fn tilde_expansion() {
        let orig = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", "/root") };
        assert_eq!(expand_tilde("~"), "/root");
        assert_eq!(expand_tilde("~/dir/file"), "/root/dir/file");
        assert_eq!(expand_tilde("~/foo"), "/root/foo");

        unsafe { std::env::remove_var("HOME") };
        assert_eq!(expand_tilde("~"), "/");

        // 恢复 HOME，避免影响同进程其他测试
        match orig {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
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

    // ─── glob 保护（引号/转义）──────────────

    #[test]
    fn glob_escape_marker_protects_metachars() {
        assert!(!has_active_glob(&format!("{}*", GLOB_ESCAPE)));
        assert!(!has_active_glob(&format!("{}?", GLOB_ESCAPE)));
        assert!(!has_active_glob(&format!("{}[ab]", GLOB_ESCAPE)));
        assert!(has_active_glob("*"));
        assert!(has_active_glob("?"));
        assert!(has_active_glob("[ab]"));
        // 混合：受保护的 * 不影响未保护的 *
        assert!(has_active_glob(&format!("{}*x*", GLOB_ESCAPE)));
        assert!(!has_active_glob("plain.txt"));
    }

    #[test]
    fn unescape_glob_removes_marker() {
        assert_eq!(unescape_glob(&format!("{}*", GLOB_ESCAPE)), "*");
        assert_eq!(unescape_glob(&format!("a{}[b", GLOB_ESCAPE)), "a[b");
        assert_eq!(unescape_glob("plain"), "plain");
        // 多个标记
        assert_eq!(
            unescape_glob(&format!("{}*{}?", GLOB_ESCAPE, GLOB_ESCAPE)),
            "*?"
        );
    }

    #[test]
    fn glob_match_respects_escape_marker() {
        assert!(glob_match(&format!("{}*", GLOB_ESCAPE), "*"));
        assert!(!glob_match(&format!("{}*", GLOB_ESCAPE), "abc"));
        assert!(glob_match(&format!("a{}*b*", GLOB_ESCAPE), "a*bcd"));
    }

    #[test]
    fn expand_pipeline_keeps_quoted_glob_literal() {
        // 回归：echo "*" / echo '*' 不应展开为目录项
        let p = Pipeline {
            cmds: vec![SimpleCmd {
                argv: vec!["echo".into(), format!("{}*", GLOB_ESCAPE)],
                ..Default::default()
            }],
            background: false,
        };
        let result = expand_pipeline(&p, 0).unwrap();
        assert_eq!(
            result.cmds[0].argv,
            vec!["echo".to_string(), "*".to_string()]
        );
    }

    #[test]
    fn expand_pipeline_keeps_mixed_word_glob() {
        // a*"b" -> 未保护的 * 仍展开（在无匹配目录下保留字面）
        let p = Pipeline {
            cmds: vec![SimpleCmd {
                argv: vec![
                    "echo".into(),
                    format!(
                        "rbox_no_such_prefix_{}*x{}y",
                        std::process::id(),
                        GLOB_ESCAPE
                    ),
                ],
                ..Default::default()
            }],
            background: false,
        };
        let result = expand_pipeline(&p, 0).unwrap();
        // 无匹配 -> 去掉保护标记后保留原词
        assert_eq!(
            result.cmds[0].argv[1],
            format!("rbox_no_such_prefix_{}*xy", std::process::id())
        );
    }

    // ─── expand_pipeline ──────────────────────

    #[test]
    fn expand_pipeline_preserves_redirects() {
        let p = Pipeline {
            cmds: vec![SimpleCmd {
                argv: vec!["echo".into(), "$VAR".into()],
                stdout_file: Some("/tmp/out".into()),
                ..Default::default()
            }],
            background: false,
        };
        unsafe {
            std::env::set_var("VAR", "hello");
        }
        let result = expand_pipeline(&p, 0).unwrap();
        assert_eq!(result.cmds[0].argv, vec!["echo", "hello"]);
        assert_eq!(result.cmds[0].stdout_file.as_deref(), Some("/tmp/out"));
        unsafe {
            std::env::remove_var("VAR");
        }
    }

    #[test]
    fn expand_pipeline_preserves_background() {
        let p = Pipeline {
            cmds: vec![SimpleCmd {
                argv: vec!["sleep".into(), "10".into()],
                ..Default::default()
            }],
            background: true,
        };
        let result = expand_pipeline(&p, 0).unwrap();
        assert!(result.background);
    }

    #[test]
    fn expand_pipeline_preserves_stderr() {
        let p = Pipeline {
            cmds: vec![SimpleCmd {
                argv: vec!["echo".into(), "hi".into()],
                stderr_file: Some("/tmp/err".into()),
                append_err: true,
                ..Default::default()
            }],
            background: false,
        };
        let result = expand_pipeline(&p, 0).unwrap();
        assert_eq!(result.cmds[0].stderr_file.as_deref(), Some("/tmp/err"));
        assert!(result.cmds[0].append_err);
    }

    #[test]
    fn expand_pipeline_preserves_heredoc() {
        let p = Pipeline {
            cmds: vec![SimpleCmd {
                argv: vec!["cat".into()],
                heredoc: Some("EOF".into()),
                ..Default::default()
            }],
            background: false,
        };
        let result = expand_pipeline(&p, 0).unwrap();
        assert_eq!(result.cmds[0].heredoc.as_deref(), Some("EOF"));
    }

    #[test]
    fn expand_pipeline_multiple_cmds() {
        let p = Pipeline {
            cmds: vec![
                SimpleCmd {
                    argv: vec!["echo".into(), "$A".into()],
                    ..Default::default()
                },
                SimpleCmd {
                    argv: vec!["cat".into()],
                    ..Default::default()
                },
            ],
            background: false,
        };
        unsafe {
            std::env::set_var("A", "x");
        }
        let result = expand_pipeline(&p, 0).unwrap();
        assert_eq!(result.cmds.len(), 2);
        assert_eq!(result.cmds[0].argv, vec!["echo", "x"]);
        assert_eq!(result.cmds[1].argv, vec!["cat"]);
        unsafe {
            std::env::remove_var("A");
        }
    }

    // ─── expand_vars 边界 ──────────────────────

    #[test]
    fn expand_vars_dollar_at_end() {
        // lone $ at end -> literal $
        assert_eq!(expand_vars("echo $", 0), "echo $");
    }

    #[test]
    fn expand_vars_unclosed_brace() {
        // ${VAR without closing } -> takes rest of string
        unsafe {
            std::env::set_var("VAR", "val");
        }
        assert_eq!(expand_vars("${VAR", 0), "val");
        unsafe {
            std::env::remove_var("VAR");
        }
    }

    #[test]
    fn expand_vars_empty_brace() {
        // ${} -> empty
        assert_eq!(expand_vars("${}", 0), "");
    }

    #[test]
    fn expand_vars_dollar_underscore() {
        unsafe {
            std::env::set_var("_", "underscore_val");
        }
        assert_eq!(expand_vars("$_", 0), "underscore_val");
        unsafe {
            std::env::remove_var("_");
        }
    }

    #[test]
    fn expand_vars_multiple() {
        unsafe {
            std::env::set_var("A", "1");
            std::env::set_var("B", "2");
        }
        assert_eq!(expand_vars("$A-$B", 0), "1-2");
        unsafe {
            std::env::remove_var("A");
            std::env::remove_var("B");
        }
    }

    // ─── expand_history 边界 ──────────────────

    #[test]
    fn hist_bang_at_end_no_next() {
        // ! at end with no next char -> literal !
        let history = vec!["echo a".to_string()];
        assert_eq!(expand_history("echo !", &history), "echo !");
    }

    #[test]
    fn hist_bang_n_out_of_range() {
        let history = vec!["echo a".to_string()];
        // !99 -> out of range, literal !99
        assert_eq!(expand_history("!99", &history), "!99");
    }

    #[test]
    fn hist_bang_minus_n_out_of_range() {
        let history = vec!["echo a".to_string()];
        assert_eq!(expand_history("!-5", &history), "!-5");
    }

    #[test]
    fn hist_bang_invalid_char() {
        let history = vec!["echo a".to_string()];
        // !x -> unknown, literal !
        assert_eq!(expand_history("!x", &history), "!x");
    }

    // ─── 参数展开运算符 ────────────────────────

    #[test]
    fn param_default_and_alternate() {
        unsafe { std::env::remove_var("RBOX_P_EMPTY") };
        unsafe { std::env::set_var("RBOX_P_SET", "val") };
        assert_eq!(expand_vars("${RBOX_P_EMPTY:-def}", 0), "def");
        assert_eq!(expand_vars("${RBOX_P_SET:-def}", 0), "val");
        assert_eq!(expand_vars("${RBOX_P_SET:+alt}", 0), "alt");
        assert_eq!(expand_vars("${RBOX_P_EMPTY:+alt}", 0), "");
        assert_eq!(expand_vars("${RBOX_P_EMPTY:=assigned}", 0), "assigned");
        assert_eq!(std::env::var("RBOX_P_EMPTY").unwrap(), "assigned");
        unsafe { std::env::remove_var("RBOX_P_EMPTY") };
        unsafe { std::env::remove_var("RBOX_P_SET") };
    }

    #[test]
    fn param_length_and_question() {
        unsafe { std::env::set_var("RBOX_P_LEN", "hello") };
        assert_eq!(expand_vars("${#RBOX_P_LEN}", 0), "5");
        unsafe { std::env::remove_var("RBOX_P_LEN") };
        let _ = crate::applets::core::shell::options::take_nounset_violation();
        assert_eq!(expand_vars("${RBOX_P_MISSING:?boom}", 0), "");
        assert!(crate::applets::core::shell::options::take_nounset_violation());
    }

    #[test]
    fn param_prefix_suffix_removal() {
        unsafe { std::env::set_var("RBOX_P_PATH", "/usr/local/bin/file.txt") };
        assert_eq!(
            expand_vars("${RBOX_P_PATH#*/}", 0),
            "usr/local/bin/file.txt"
        );
        assert_eq!(expand_vars("${RBOX_P_PATH##*/}", 0), "file.txt");
        assert_eq!(expand_vars("${RBOX_P_PATH%/*}", 0), "/usr/local/bin");
        assert_eq!(expand_vars("${RBOX_P_PATH%%/*}", 0), "");
        unsafe { std::env::remove_var("RBOX_P_PATH") };
    }

    #[test]
    fn param_pattern_with_quoted_star() {
        unsafe { std::env::set_var("RBOX_P_Q", "/a/b/c.txt") };
        // 模式中的 * 带引号保护标记（如 "${p##*/}"），应仍按 glob 匹配
        let pat = format!("{}*/", GLOB_ESCAPE);
        assert_eq!(expand_braced(&format!("RBOX_P_Q##{}", pat), 0), "c.txt");
        let pat2 = format!("/{}*", GLOB_ESCAPE);
        assert_eq!(expand_braced(&format!("RBOX_P_Q%{}", pat2), 0), "/a/b");
        unsafe { std::env::remove_var("RBOX_P_Q") };
    }

    #[test]
    fn param_replacement() {
        unsafe { std::env::set_var("RBOX_P_REP", "a-b-c") };
        assert_eq!(expand_vars("${RBOX_P_REP/-/_}", 0), "a_b-c");
        assert_eq!(expand_vars("${RBOX_P_REP//-/_}", 0), "a_b_c");
        unsafe { std::env::remove_var("RBOX_P_REP") };
    }

    // ─── 词分割与 "$@" ─────────────────────────

    #[test]
    fn word_splitting_unquoted() {
        unsafe { std::env::set_var("RBOX_SPLIT", "a b  c") };
        let words = expand_word(&format!("{}$RBOX_SPLIT", SPLIT_ESCAPE), 0);
        assert_eq!(words, vec!["a", "b", "c"]);
        unsafe { std::env::remove_var("RBOX_SPLIT") };
    }

    #[test]
    fn quoted_expansion_not_split() {
        unsafe { std::env::set_var("RBOX_SPLIT", "a b") };
        let words = expand_word(&format!("{}${}", NO_SPLIT_ESCAPE, "RBOX_SPLIT"), 0);
        assert_eq!(words, vec!["a b"]);
        unsafe { std::env::remove_var("RBOX_SPLIT") };
    }

    #[test]
    fn quoted_at_expands_multiple_words() {
        super::super::params::set(vec!["one".into(), "two three".into()]);
        let words = expand_word(&format!("{}$@", NO_SPLIT_ESCAPE), 0);
        assert_eq!(words, vec!["one", "two three"]);
        super::super::params::set(Vec::new());
    }

    // ─── 花括号展开 ────────────────────────────

    #[test]
    fn brace_expansion_list_and_range() {
        assert_eq!(expand_word("a{b,c}d", 0), vec!["abd", "acd"]);
        assert_eq!(expand_word("{1..3}", 0), vec!["1", "2", "3"]);
        assert_eq!(expand_word("x{a..c}", 0), vec!["xa", "xb", "xc"]);
        assert_eq!(expand_word("{3..1}", 0), vec!["3", "2", "1"]);
        // 无逗号/范围：原样
        assert_eq!(expand_word("{abc}", 0), vec!["{abc}"]);
        // 引号保护的花括号不展开
        let protected = format!("a{}{{b,c}}", GLOB_ESCAPE);
        assert_eq!(expand_word(&protected, 0), vec!["a{b,c}"]);
    }

    // ─── 算术增强 ──────────────────────────────

    #[test]
    fn arithmetic_assignment_and_compare() {
        unsafe { std::env::set_var("RBOX_A", "5") };
        assert_eq!(eval_arith("RBOX_A + 3"), Some(8));
        assert_eq!(eval_arith("RBOX_A = 7"), Some(7));
        assert_eq!(std::env::var("RBOX_A").unwrap(), "7");
        assert_eq!(eval_arith("RBOX_A += 2"), Some(9));
        assert_eq!(eval_arith("RBOX_A == 9"), Some(1));
        assert_eq!(eval_arith("RBOX_A > 100"), Some(0));
        assert_eq!(eval_arith("RBOX_A < 100 && RBOX_A > 0"), Some(1));
        assert_eq!(eval_arith("!(RBOX_A == 0)"), Some(1));
        unsafe { std::env::remove_var("RBOX_A") };
    }

    // ─── expand_tilde 边界 ────────────────────
    //（tilde_no_home_var / tilde_with_home_set 已并入 tilde_expansion，避免共享 HOME 竞争）

    // ─── ash 对齐：算术位运算/三元/自增、子串、$- ────────────
    #[test]
    fn arith_bitwise_ternary() {
        assert_eq!(expand_vars("$((5&3))", 0), "1");
        assert_eq!(expand_vars("$((5|2))", 0), "7");
        assert_eq!(expand_vars("$((5^1))", 0), "4");
        assert_eq!(expand_vars("$((~0))", 0), "-1");
        assert_eq!(expand_vars("$((1?2:3))", 0), "2");
        assert_eq!(expand_vars("$((0?2:3))", 0), "3");
        assert_eq!(expand_vars("$((1,2,3))", 0), "3");
    }

    #[test]
    fn arith_power() {
        assert_eq!(expand_vars("$((2**10))", 0), "1024");
        // 右结合：2**(3**2) = 512
        assert_eq!(expand_vars("$((2**3**2))", 0), "512");
        // 一元负号作用于整个幂：-(2**2)
        assert_eq!(expand_vars("$((-2**2))", 0), "-4");
        assert_eq!(expand_vars("$((2*3**2))", 0), "18");
        // 负指数非法 → 0
        assert_eq!(expand_vars("$((2**-1))", 0), "0");
    }

    #[test]
    fn arith_incdec() {
        // SAFETY: 单测串行（cargo test 单线程默认并发，使用唯一变量名避免竞争）
        unsafe {
            std::env::set_var("RBOX_T_INC_A", "5");
        }
        assert_eq!(expand_vars("$((RBOX_T_INC_A++))", 0), "5");
        assert_eq!(std::env::var("RBOX_T_INC_A").unwrap(), "6");
        assert_eq!(expand_vars("$((++RBOX_T_INC_A))", 0), "7");
        assert_eq!(std::env::var("RBOX_T_INC_A").unwrap(), "7");
    }

    #[test]
    fn param_substring() {
        // SAFETY: 单测使用唯一变量名
        unsafe {
            std::env::set_var("RBOX_T_SUB", "abcdef");
        }
        assert_eq!(expand_vars("${RBOX_T_SUB:1:3}", 0), "bcd");
        assert_eq!(expand_vars("${RBOX_T_SUB:2}", 0), "cdef");
    }

    #[test]
    fn dollar_dash_options() {
        let out = expand_vars("$-", 0);
        // 至少应包含 -e/-x 等字符或为空，不产生 `$` 字面
        assert!(!out.contains('$'));
    }

    #[test]
    fn param_substring_edges() {
        // SAFETY: 单测使用唯一变量名
        unsafe {
            std::env::set_var("RBOX_T_SUB3", "abcdef");
        }
        assert_eq!(expand_vars("${RBOX_T_SUB3:1}", 0), "bcdef");
        assert_eq!(expand_vars("${RBOX_T_SUB3: -2}", 0), "ef");
        assert_eq!(expand_vars("${RBOX_T_SUB3:10}", 0), "");
        assert_eq!(expand_vars("${RBOX_T_SUB3:0:0}", 0), "");
    }

    #[test]
    fn random_expands_to_number() {
        for _ in 0..5 {
            let out = expand_vars("$RANDOM", 0);
            let n: u32 = out.parse().expect("$RANDOM 应为数字");
            assert!(n < 65536, "$RANDOM 越界: {}", n);
        }
    }

    #[test]
    fn mixed_quote_field_split() {
        unsafe { std::env::set_var("RBOX_T_MIX", "1 2") };
        // 未加引号展开参与拆分；引号部分保留
        assert_eq!(
            expand_word(&format!("ab c{}$RBOX_T_MIX", SPLIT_ESCAPE), 0),
            vec!["ab c1", "2"]
        );
        // 双引号内展开不拆分（前后字面保留）
        assert_eq!(
            expand_word(
                &format!("pre{}$RBOX_T_MIX{}suf", NO_SPLIT_ESCAPE, NO_SPLIT_ESCAPE),
                0
            ),
            vec!["pre1 2suf"]
        );
        unsafe { std::env::remove_var("RBOX_T_MIX") };
    }
}
