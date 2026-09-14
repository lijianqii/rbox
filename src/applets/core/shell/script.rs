//! 脚本执行：`sh script [args]`、`sh -c 'cmd'`、`source`、函数体、`eval`。
//!
//! 与交互式 REPL 共用 `executor::execute_line` / `compound::execute_block`，
//! 但输入来自字符串/文件，且支持脚本选项（-e/-x/-u/-o pipefail）、
//! 函数定义、here-doc、`return`/`exit` 传播。

use super::executor::execute_line;
use super::{compound, executor, functions, options, params, trap};

/// `sh` 命令行解析结果。
#[derive(Debug, Default, PartialEq)]
pub(crate) struct ShellArgs {
    /// `-c` 命令字符串。
    pub(crate) command: Option<String>,
    /// 脚本文件。
    pub(crate) script: Option<String>,
    /// 位置参数。
    pub(crate) args: Vec<String>,
    /// 强制交互（`-i`）。
    pub(crate) interactive: bool,
}

/// 解析 shell 参数；未知选项返回 Err。
pub(crate) fn parse_shell_args(args: &[String]) -> Result<ShellArgs, String> {
    let mut out = ShellArgs::default();
    let mut i = 0;
    let mut end_of_options = false;
    while i < args.len() {
        let a = &args[i];
        if !end_of_options && a == "--" {
            end_of_options = true;
            i += 1;
            continue;
        }
        if !end_of_options && a.starts_with('-') && a.len() > 1 {
            let chars: Vec<char> = a[1..].chars().collect();
            let mut j = 0;
            while j < chars.len() {
                match chars[j] {
                    'c' => {
                        i += 1;
                        let Some(cmd) = args.get(i) else {
                            return Err("-c requires an argument".to_string());
                        };
                        out.command = Some(cmd.clone());
                    }
                    'e' => options::set_errexit(true),
                    'x' => options::set_xtrace(true),
                    'u' => options::set_nounset(true),
                    'i' => out.interactive = true,
                    's' => {} // 读 stdin（默认行为）
                    'o' => {
                        i += 1;
                        let Some(name) = args.get(i) else {
                            return Err("-o requires an argument".to_string());
                        };
                        match name.as_str() {
                            "pipefail" => options::set_pipefail(true),
                            other => return Err(format!("unknown option: -o {}", other)),
                        }
                    }
                    other => return Err(format!("unknown option: -{}", other)),
                }
                j += 1;
            }
            i += 1;
            continue;
        }
        if out.script.is_none() && out.command.is_none() {
            out.script = Some(a.clone());
        } else {
            out.args.push(a.clone());
        }
        i += 1;
    }
    Ok(out)
}

/// 续行判断（复用 REPL 的实现）。
pub(crate) fn needs_continuation(line: &str) -> bool {
    super::needs_continuation(line)
}

/// 函数定义头解析：`name() {` / `name ()` / `function name {`。
/// 返回 (函数名, 头部 `{` 之后的剩余文本)；不是定义返回 None。
pub(crate) fn parse_function_header(line: &str) -> Option<(String, String)> {
    let t = line.trim_start();
    let (rest, name) = if let Some(r) = t.strip_prefix("function ") {
        let r = r.trim_start();
        let end = r
            .find(|c: char| c.is_whitespace() || c == '{')
            .unwrap_or(r.len());
        (r[end..].trim_start(), r[..end].to_string())
    } else {
        // name() 形式
        let paren = t.find('(')?;
        let name = t[..paren].trim();
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return None;
        }
        let after = t[paren..].trim_start();
        if !after.starts_with("()") {
            return None;
        }
        (after[2..].trim_start(), name.to_string())
    };
    if !rest.starts_with('{') {
        return None;
    }
    Some((name, rest[1..].to_string()))
}

/// 内联函数体：找顶层 `}`，返回 (函数体, 后续命令)。
fn split_inline_brace(s: &str) -> Option<(String, Option<String>)> {
    let bytes = s.as_bytes();
    let mut depth: i32 = 0;
    let mut in_sq = false;
    let mut in_dq = false;
    for (i, &c) in bytes.iter().enumerate() {
        if in_sq {
            if c == b'\'' {
                in_sq = false;
            }
            continue;
        }
        if in_dq {
            if c == b'"' {
                in_dq = false;
            }
            continue;
        }
        match c {
            b'\'' => in_sq = true,
            b'"' => in_dq = true,
            b'{' => depth += 1,
            b'}' => {
                if depth == 0 {
                    let inner = s[..i].trim().to_string();
                    let rest = s[i + 1..].trim().trim_start_matches(';').trim();
                    return Some((inner, (!rest.is_empty()).then(|| rest.to_string())));
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

/// 收集函数定义体：处理内联 `{ ... }` 与跨行 `{` ... `}`。
/// 返回 (函数名, 函数体文本, 额外消耗的行数)。
pub(crate) fn collect_function(
    line: &str,
    rest_lines: &[String],
) -> Option<(String, String, usize, Option<String>)> {
    let (name, after) = parse_function_header(line)?;
    let after = after.trim();
    // 内联：`{ cmd; }` 或 `{ cmd; }; 后续命令`
    if let Some((inner, tail)) = split_inline_brace(after) {
        return Some((name, inner, 0, tail));
    }
    let mut body = String::new();
    if !after.is_empty() {
        body.push_str(after);
        body.push('\n');
    }
    let mut consumed = 0;
    for l in rest_lines {
        consumed += 1;
        let t = l.trim();
        if t == "}" {
            return Some((name, body, consumed, None));
        }
        if let Some(inner) = t.strip_suffix('}') {
            body.push_str(inner);
            body.push('\n');
            return Some((name, body, consumed, None));
        }
        body.push_str(l);
        body.push('\n');
    }
    None
}

/// here-doc 临时文件序号。
static HEREDOC_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// 展开 here-doc 内容（未加引号的 delimiter 时展开变量与命令替换）。
pub(crate) fn expand_heredoc_body(body: &str, expand: bool) -> String {
    if !expand {
        return body.to_string();
    }
    let with_cmd = executor::expand_command_subst(body);
    let mut out = String::new();
    for line in with_cmd.lines() {
        out.push_str(&super::expander::expand_vars_clean(line, 0));
        out.push('\n');
    }
    out
}

/// 收集 here-doc：返回 (替换后的行, 额外消耗的行数)。
/// 支持 `<<DELIM`、`<<-DELIM`（去 tab）与引号 delimiter（不展开）。
pub(crate) fn collect_heredoc(line: &str, rest_lines: &[String]) -> Option<(String, usize)> {
    let idx = super::find_heredoc_operator(line)?;
    let after = line[idx + 2..].trim_start();
    let (strip_tabs, delim_raw) = match after.strip_prefix('-') {
        Some(r) => (true, r.trim_start()),
        None => (false, after),
    };
    let delim = delim_raw.split_whitespace().next().unwrap_or("");
    if delim.is_empty() {
        return None;
    }
    let quoted = (delim.starts_with('\'') && delim.ends_with('\''))
        || (delim.starts_with('"') && delim.ends_with('"'));
    let delim_clean = delim.trim_matches(|c| c == '\'' || c == '"');
    let mut body = String::new();
    let mut consumed = 0;
    for l in rest_lines {
        consumed += 1;
        let candidate = if strip_tabs {
            l.trim_start_matches('\t')
        } else {
            l.as_str()
        };
        if candidate == delim_clean {
            let expanded = expand_heredoc_body(&body, !quoted);
            let seq = HEREDOC_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let tmpfile = format!("/tmp/rbox_heredoc_{}_{}", std::process::id(), seq);
            if std::fs::write(&tmpfile, expanded).is_err() {
                return None;
            }
            let before = &line[..idx];
            return Some((format!("{} < {}", before.trim_end(), tmpfile), consumed));
        }
        body.push_str(candidate);
        body.push('\n');
    }
    None
}

/// 执行一段脚本文本（`-c` / source / 函数体 / eval 共用）。
/// `interactive=false` 时应用 -e/-u 退出语义；返回退出码。
pub(crate) fn run_source(
    source: &str,
    history: &[String],
    exit_fn: &dyn Fn(i32),
    interactive: bool,
) -> i32 {
    // 脚本模式也需捕获 INT/TERM 以执行 trap（幂等）
    trap::install_handlers();
    crate::applets::core::shell::builtin::init_pwd();
    let lines: Vec<String> = source.lines().map(|s| s.to_string()).collect();
    let mut i = 0;
    let mut last_rc = 0;
    while i < lines.len() {
        // 信号 trap：脚本模式下检查待处理信号
        if let Some(sig) = trap::take_pending() {
            match trap::get(sig) {
                Some(cmdline) => {
                    last_rc = execute_line(&cmdline, &mut last_rc, history, exit_fn);
                }
                None => {
                    // 无 trap：INT 退出 130，TERM/HUP 退出 128+sig
                    return 128 + sig;
                }
            }
        }
        let mut line = lines[i].clone();
        i += 1;
        // 续行合并
        while needs_continuation(&line) && i < lines.len() {
            let next = lines[i].clone();
            i += 1;
            line = format!("{}{}", line.trim_end_matches('\\'), next);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // verbose（set -v）：回显输入行
        if options::verbose() {
            eprintln!("{}", line);
        }
        // `!` 取反（仅作用于第一个 pipeline）
        let mut negate_tail = String::new();
        let negate = trimmed == "!" || trimmed.starts_with("! ");
        if negate {
            let rest = trimmed[1..].trim_start();
            match split_first_connector(rest) {
                Some(idx) => {
                    negate_tail = rest[idx..].to_string();
                    line = rest[..idx].trim_end().to_string();
                }
                None => line = rest.to_string(),
            }
        }
        // noexec（set -n）：仅语法检查
        if options::noexec() {
            let _ = super::parser::build_command_list(&super::tokenizer::tokenize(&line));
            continue;
        }
        // here-doc 收集
        if line.contains("<<")
            && let Some((new_line, consumed)) = collect_heredoc(&line, &lines[i..])
        {
            line = new_line;
            i += consumed;
        }
        // 函数定义
        if let Some((name, body, consumed, tail)) = collect_function(&line, &lines[i..]) {
            functions::define(&name, &body);
            i += consumed;
            if let Some(t) = tail {
                last_rc = execute_line(&t, &mut last_rc, history, exit_fn);
            }
            continue;
        }
        // `cmd && ( ... )` / `cmd || { ...; }`：条件执行组
        if let Some((op_pos, group_pos, is_and)) = find_operator_group(&line) {
            let head = line[..op_pos].to_string();
            let rc = if head.trim().is_empty() {
                last_rc
            } else {
                execute_line(&head, &mut last_rc, history, exit_fn)
            };
            let run_group = if is_and { rc == 0 } else { rc != 0 };
            line = if run_group {
                line[group_pos..].to_string()
            } else {
                String::new()
            };
        }
        // 顶层分号 + 复合关键字/子 shell：先执行前段，余下部分按复合命令处理
        {
            let segments = split_top_level_semicolons(&line);
            if segments.len() > 1 {
                let mut split_at = None;
                for (idx, seg) in segments.iter().enumerate().skip(1) {
                    let t = seg.trim_start();
                    if t.starts_with('(')
                        || t.starts_with('{')
                        || t.starts_with("! ")
                        || starts_compound_keyword(t)
                    {
                        split_at = Some(idx);
                        break;
                    }
                }
                if let Some(idx) = split_at {
                    let head: String = segments[..idx].join(";");
                    let tail: String = segments[idx..].join(";");
                    if !head.trim().is_empty() {
                        last_rc = execute_line(&head, &mut last_rc, history, exit_fn);
                    }
                    line = tail;
                }
            }
        }
        let mut rc;
        let starts_subshell = line.trim_start().starts_with('(')
            && extract_subshell(line.trim_start())
                .map(|(_, tail)| {
                    let t = tail.trim_start();
                    !(t.starts_with('|')
                        || t.starts_with("&&")
                        || t.starts_with("||")
                        || t.starts_with('&'))
                })
                .unwrap_or(true);
        if starts_subshell {
            // 子 shell：fork 执行，隔离变量与 cwd
            let mut text = line.clone();
            let mut depth = paren_delta(&text);
            while depth > 0 && i < lines.len() {
                let l = lines[i].clone();
                i += 1;
                depth += paren_delta(&l);
                text.push('\n');
                text.push_str(&l);
            }
            let Some((inner, tail)) = extract_subshell(&text) else {
                eprintln!("shell: syntax error: missing ')'");
                return 2;
            };
            let (rtail, rest) = split_redirect_tail(&tail);
            let rcmd = executor::parse_redirect_tail(&rtail);
            rc = run_subshell(&inner, history, exit_fn, rcmd.as_ref());
            if !rest.is_empty() {
                last_rc = rc;
                last_rc = execute_line(&rest, &mut last_rc, history, exit_fn);
                rc = last_rc;
            }
        } else if line.trim_start().starts_with('{') {
            // 花括号组：当前 shell 执行（变量保留）
            let mut text = line.clone();
            let mut depth = brace_delta(&text);
            while depth > 0 && i < lines.len() {
                let l = lines[i].clone();
                i += 1;
                depth += brace_delta(&l);
                text.push('\n');
                text.push_str(&l);
            }
            let Some((inner, tail)) = extract_group(&text) else {
                eprintln!("shell: syntax error: missing '}}'");
                return 2;
            };
            let (rtail, rest) = split_redirect_tail(&tail);
            let rcmd = executor::parse_redirect_tail(&rtail);
            let guard = match rcmd.as_ref() {
                Some(c) => match executor::apply_redirects(c) {
                    Ok(g) => g,
                    Err(code) => {
                        last_rc = code;
                        continue;
                    }
                },
                None => None,
            };
            rc = compound::execute_block(&inner, &mut last_rc, history, exit_fn);
            drop(guard);
            if !rest.is_empty() {
                last_rc = rc;
                last_rc = execute_line(&rest, &mut last_rc, history, exit_fn);
                rc = last_rc;
            }
        } else if compound::is_compound_start(&line) {
            // 复合命令块
            let mut depth = compound::nesting_delta(&line);
            let mut block = vec![line.clone()];
            while depth > 0 && i < lines.len() {
                let l = lines[i].clone();
                i += 1;
                depth += compound::nesting_delta(&l);
                block.push(l);
            }
            if depth > 0 {
                eprintln!("shell: syntax error: unexpected EOF");
                return 2;
            }
            rc = run_compound_lines(&mut block, last_rc, history, exit_fn);
        } else {
            rc = execute_line(&line, &mut last_rc, history, exit_fn);
        }
        last_rc = if negate {
            if rc == 0 { 1 } else { 0 }
        } else {
            rc
        };
        if !negate_tail.is_empty() {
            last_rc = execute_line(&negate_tail, &mut last_rc, history, exit_fn);
        }
        // 行执行后检查待处理信号 trap（信号可能在命令执行期间到达）
        if let Some(sig) = trap::take_pending() {
            match trap::get(sig) {
                Some(cmdline) => {
                    last_rc = execute_line(&cmdline, &mut last_rc, history, exit_fn);
                }
                None => return 128 + sig,
            }
        }
        // return：本帧消费
        if let Some(code) = options::take_return() {
            return code;
        }
        // nounset（-u）：未定义变量展开后退出
        if options::take_nounset_violation() {
            return 1;
        }
        // exit 请求（set -e / nounset / 显式）
        if let Some(code) = options::take_exit_request() {
            return code;
        }
        // -e：非条件行失败则退出（`&&`/`||`/`!` 行由 bash 豁免，这里近似）
        if !interactive && options::errexit() && last_rc != 0 && !has_conditional(&line) {
            return last_rc;
        }
    }
    last_rc
}

/// 行内是否含 `&&`/`||`/`!`（-e 豁免判断，引号感知）。
fn has_conditional(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_squote = false;
    let mut in_dquote = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_squote {
            if c == b'\'' {
                in_squote = false;
            }
            i += 1;
            continue;
        }
        if in_dquote {
            match c {
                b'"' => in_dquote = false,
                b'\\' => i += 1,
                _ => {}
            }
            i += 1;
            continue;
        }
        match c {
            b'\'' => in_squote = true,
            b'"' => in_dquote = true,
            b'\\' => i += 1,
            b'&' | b'|' if bytes.get(i + 1) == Some(&c) => return true,
            b'!' => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

/// 顶层分号分段（引号感知）。
fn split_top_level_semicolons(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    let mut in_squote = false;
    let mut in_dquote = false;
    let mut in_backtick = false;
    let mut depth: i32 = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_squote {
            cur.push(c as char);
            if c == b'\'' {
                in_squote = false;
            }
            i += 1;
            continue;
        }
        if in_dquote {
            if c == b'\\' && i + 1 < bytes.len() {
                cur.push('\\');
                cur.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            cur.push(c as char);
            if c == b'"' {
                in_dquote = false;
            }
            i += 1;
            continue;
        }
        if in_backtick {
            cur.push(c as char);
            if c == b'\\' && i + 1 < bytes.len() {
                cur.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if c == b'`' {
                in_backtick = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'\'' => {
                in_squote = true;
                cur.push('\'');
            }
            b'"' => {
                in_dquote = true;
                cur.push('"');
            }
            b'`' => {
                in_backtick = true;
                cur.push('`');
            }
            b'(' | b'{' => {
                depth += 1;
                cur.push(c as char);
            }
            b')' | b'}' => {
                depth -= 1;
                cur.push(c as char);
            }
            b';' if depth == 0 => {
                parts.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c as char),
        }
        i += 1;
    }
    parts.push(cur);
    parts
}

/// 查找顶层 `&&`/`||` 后紧跟 `(`/`{` 的位置：返回 (op_pos, group_pos, is_and)。
fn find_operator_group(line: &str) -> Option<(usize, usize, bool)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut depth: i32 = 0;
    let mut in_squote = false;
    let mut in_dquote = false;
    let mut in_backtick = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_squote {
            if c == b'\'' {
                in_squote = false;
            }
            i += 1;
            continue;
        }
        if in_dquote {
            if c == b'\\' {
                i += 1;
            } else if c == b'"' {
                in_dquote = false;
            }
            i += 1;
            continue;
        }
        if in_backtick {
            if c == b'`' {
                in_backtick = false;
            } else if c == b'\\' {
                i += 1;
            }
            i += 1;
            continue;
        }
        match c {
            b'\'' => in_squote = true,
            b'"' => in_dquote = true,
            b'`' => in_backtick = true,
            b'(' | b'{' => depth += 1,
            b')' | b'}' => depth -= 1,
            b'&' | b'|' if depth == 0 => {
                let is_and = c == b'&';
                let op_len = if bytes.get(i + 1) == Some(&c) { 2 } else { 0 };
                if op_len == 2 {
                    let mut j = i + 2;
                    while bytes.get(j).is_some_and(|b| *b == b' ' || *b == b'\t') {
                        j += 1;
                    }
                    if matches!(bytes.get(j), Some(b'(') | Some(b'{')) {
                        return Some((i, j, is_and));
                    }
                }
                i += op_len.max(1);
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// 是否为复合命令起始关键字。
fn starts_compound_keyword(t: &str) -> bool {
    let w = t.split_whitespace().next().unwrap_or("");
    matches!(w, "for" | "while" | "until" | "if" | "case" | "select")
}

/// 拆分重定向尾部与后续命令：`< file; echo x` -> (`< file`, `; echo x`)。
fn split_redirect_tail(tail: &str) -> (String, String) {
    match split_first_connector(tail) {
        Some(i) => (tail[..i].to_string(), tail[i..].to_string()),
        None => (tail.to_string(), String::new()),
    }
}

/// 执行复合命令块（含终结符行重定向，如 `done < file`）。
pub(crate) fn run_compound_lines(
    block: &mut [String],
    last_rc: i32,
    history: &[String],
    exit_fn: &dyn Fn(i32),
) -> i32 {
    let mut last_rc = last_rc;
    let term = match first_word(block.first().map(String::as_str).unwrap_or("")) {
        "if" => "fi",
        "case" => "esac",
        _ => "done",
    };
    let mut tail = String::new();
    if let Some(last) = block.last_mut()
        && let Some((rest, t)) = split_terminator_tail(last, term)
    {
        *last = rest;
        tail = t;
    }
    let (rtail, rest) = split_redirect_tail(&tail);
    let rcmd = executor::parse_redirect_tail(&rtail);
    let guard = match rcmd.as_ref() {
        Some(c) => match executor::apply_redirects(c) {
            Ok(g) => g,
            Err(code) => return code,
        },
        None => None,
    };
    let rc = compound::execute_block(&block.join("\n"), &mut last_rc, history, exit_fn);
    drop(guard);
    if !rest.is_empty() {
        last_rc = rc;
        return execute_line(&rest, &mut last_rc, history, exit_fn);
    }
    rc
}

/// 交互式单行执行：支持 `cmd; ( ... )`、`cmd; { ...; }`、`cmd; for ...; done` 等形式。
pub(crate) fn run_interactive_line(
    line: &str,
    last_rc: i32,
    history: &[String],
    exit_fn: &dyn Fn(i32),
) -> i32 {
    let mut line = line.to_string();
    let mut rc = last_rc;
    if let Some((op_pos, group_pos, is_and)) = find_operator_group(&line) {
        let head = line[..op_pos].to_string();
        let r = if head.trim().is_empty() {
            rc
        } else {
            execute_line(&head, &mut rc, history, exit_fn)
        };
        let run_group = if is_and { r == 0 } else { r != 0 };
        line = if run_group {
            line[group_pos..].to_string()
        } else {
            String::new()
        };
    }
    {
        let segments = split_top_level_semicolons(&line);
        if segments.len() > 1 {
            let mut split_at = None;
            for (idx, seg) in segments.iter().enumerate().skip(1) {
                let t = seg.trim_start();
                if t.starts_with('(')
                    || t.starts_with('{')
                    || t.starts_with("! ")
                    || starts_compound_keyword(t)
                {
                    split_at = Some(idx);
                    break;
                }
            }
            if let Some(idx) = split_at {
                let head: String = segments[..idx].join(";");
                let tail: String = segments[idx..].join(";");
                if !head.trim().is_empty() {
                    rc = execute_line(&head, &mut rc, history, exit_fn);
                }
                line = tail;
            }
        }
    }
    let t = line.trim_start();
    let pure_group = if t.starts_with('(') {
        extract_subshell(t)
            .map(|(_, tail)| {
                let tt = tail.trim_start();
                !(tt.starts_with('|')
                    || tt.starts_with("&&")
                    || tt.starts_with("||")
                    || tt.starts_with('&'))
            })
            .unwrap_or(true)
    } else {
        t.starts_with('{')
    };
    if pure_group || t.starts_with("! ") {
        return run_segment(&line, history, exit_fn, rc);
    }
    if starts_compound_keyword(t) {
        let mut block = vec![line.clone()];
        return run_compound_lines(&mut block, rc, history, exit_fn);
    }
    execute_line(&line, &mut rc, history, exit_fn)
}

/// 执行单个顶层段（处理 `!`、`(` 子 shell、`{` 组与普通命令）。
fn run_segment(seg: &str, history: &[String], exit_fn: &dyn Fn(i32), mut last_rc: i32) -> i32 {
    let t = seg.trim();
    if t.is_empty() {
        return last_rc;
    }
    let mut rc = last_rc;
    let negate = t.starts_with("! ") || t == "!";
    let body = if negate { t[1..].trim_start() } else { t };
    if negate && let Some(idx) = split_first_connector(body) {
        let first = body[..idx].trim_end();
        let rest = body[idx..].to_string();
        let mut r = execute_line(first, &mut rc, history, exit_fn);
        r = if r == 0 { 1 } else { 0 };
        return execute_line(&rest, &mut r, history, exit_fn);
    }
    if body.starts_with('(') {
        if let Some((inner, tail)) = extract_subshell(body) {
            let (rtail, rest) = split_redirect_tail(&tail);
            let rcmd = executor::parse_redirect_tail(&rtail);
            rc = run_subshell(&inner, history, exit_fn, rcmd.as_ref());
            if !rest.is_empty() {
                last_rc = rc;
                last_rc = execute_line(&rest, &mut last_rc, history, exit_fn);
                rc = last_rc;
            }
        } else {
            eprintln!("shell: syntax error: missing ')'");
            rc = 2;
        }
    } else if body.starts_with('{') {
        if let Some((inner, tail)) = extract_group(body) {
            let (rtail, rest) = split_redirect_tail(&tail);
            let rcmd = executor::parse_redirect_tail(&rtail);
            let guard = match rcmd.as_ref() {
                Some(c) => executor::apply_redirects(c).ok().flatten(),
                None => None,
            };
            rc = compound::execute_block(&inner, &mut rc, history, exit_fn);
            drop(guard);
            if !rest.is_empty() {
                rc = execute_line(&rest, &mut rc, history, exit_fn);
            }
        } else {
            eprintln!("shell: syntax error: missing '}}'");
            rc = 2;
        }
    } else {
        rc = execute_line(body, &mut rc, history, exit_fn);
    }
    if negate {
        if rc == 0 { 1 } else { 0 }
    } else {
        rc
    }
}

/// 找第一个顶层连接符（`;`、`&`、`&&`、`||`）的字节位置（引号感知）。
fn split_first_connector(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_squote = false;
    let mut in_dquote = false;
    let mut in_backtick = false;
    let mut depth: i32 = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_squote {
            if c == b'\'' {
                in_squote = false;
            }
            i += 1;
            continue;
        }
        if in_dquote {
            match c {
                b'"' => in_dquote = false,
                b'\\' => i += 1,
                _ => {}
            }
            i += 1;
            continue;
        }
        if in_backtick {
            if c == b'`' {
                in_backtick = false;
            } else if c == b'\\' {
                i += 1;
            }
            i += 1;
            continue;
        }
        match c {
            b'\'' => in_squote = true,
            b'"' => in_dquote = true,
            b'`' => in_backtick = true,
            b'(' | b'{' => depth += 1,
            b')' | b'}' => depth -= 1,
            b'\\' => i += 1,
            b';' | b'&' | b'|' if depth == 0 => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// 行内未加引号的括号深度增量（`(` +1，`)` -1）。
fn paren_delta(line: &str) -> i32 {
    let bytes = line.as_bytes();
    let mut depth = 0;
    let mut i = 0;
    let mut in_squote = false;
    let mut in_dquote = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_squote {
            if c == b'\'' {
                in_squote = false;
            }
            i += 1;
            continue;
        }
        if in_dquote {
            match c {
                b'"' => in_dquote = false,
                b'\\' => i += 1,
                _ => {}
            }
            i += 1;
            continue;
        }
        match c {
            b'\'' => in_squote = true,
            b'"' => in_dquote = true,
            b'\\' => i += 1,
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    depth
}

/// 行内独立 `{`/`}` 记号深度增量。
fn brace_delta(line: &str) -> i32 {
    let mut depth = 0;
    for tok in line.split(|c: char| c.is_whitespace() || c == ';') {
        match tok {
            "{" => depth += 1,
            "}" => depth -= 1,
            _ => {}
        }
    }
    depth
}

/// 提取子 shell 内容：`( inner ) tail` -> (inner, tail)。
fn extract_subshell(text: &str) -> Option<(String, String)> {
    let start = text.find('(')?;
    let bytes = text.as_bytes();
    let mut depth = 0;
    let mut i = start;
    let mut in_squote = false;
    let mut in_dquote = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_squote {
            if c == b'\'' {
                in_squote = false;
            }
            i += 1;
            continue;
        }
        if in_dquote {
            match c {
                b'"' => in_dquote = false,
                b'\\' => i += 1,
                _ => {}
            }
            i += 1;
            continue;
        }
        match c {
            b'\'' => in_squote = true,
            b'"' => in_dquote = true,
            b'\\' => i += 1,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((text[start + 1..i].to_string(), text[i + 1..].to_string()));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// 提取花括号组内容：`{ inner; } tail` -> (inner, tail)。
fn extract_group(text: &str) -> Option<(String, String)> {
    let start = text.find('{')?;
    let bytes = text.as_bytes();
    let mut depth = 0;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((text[start + 1..i].to_string(), text[i + 1..].to_string()));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// 拆分终结符行：`done < file` -> ("done", "< file")。
fn split_terminator_tail(line: &str, term: &str) -> Option<(String, String)> {
    let bytes = line.as_bytes();
    let tb = term.as_bytes();
    let mut i = 0;
    while i + tb.len() <= bytes.len() {
        if &bytes[i..i + tb.len()] == tb {
            let prev_ok = i == 0 || bytes[i - 1].is_ascii_whitespace() || bytes[i - 1] == b';';
            let next_ok = i + tb.len() == bytes.len() || bytes[i + tb.len()].is_ascii_whitespace();
            if prev_ok && next_ok {
                return Some((
                    line[..i + tb.len()].trim_end().to_string(),
                    line[i + tb.len()..].trim_start().to_string(),
                ));
            }
        }
        i += 1;
    }
    None
}

/// 取一行首词（用于判断块终结符）。
fn first_word(line: &str) -> &str {
    line.split_whitespace().next().unwrap_or("")
}

/// 子 shell：fork 后在子进程执行（隔离变量/cwd/陷阱），父进程等待。
fn run_subshell(
    inner: &str,
    history: &[String],
    exit_fn: &dyn Fn(i32),
    redirect: Option<&crate::applets::core::shell::types::SimpleCmd>,
) -> i32 {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        eprintln!("shell: fork failed");
        return 1;
    }
    if pid == 0 {
        // 子进程
        trap::reset_for_subshell();
        let _guard = match redirect {
            Some(c) => match executor::apply_redirects(c) {
                Ok(g) => g,
                Err(code) => std::process::exit(code & 0xff),
            },
            None => None,
        };
        let rc = run_source(inner, history, exit_fn, false);
        std::process::exit(rc & 0xff);
    }
    let mut status: libc::c_int = 0;
    unsafe {
        libc::waitpid(pid, &mut status, 0);
    }
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        128 + libc::WTERMSIG(status)
    }
}

/// 运行 EXIT trap（`trap 'cmd' EXIT`）。
pub(crate) fn run_exit_trap(history: &[String], last_rc: i32) {
    if let Some(cmdline) = trap::get(0) {
        trap::clear(0);
        let mut rc = last_rc;
        execute_line(&cmdline, &mut rc, history, &|_| {});
    }
}

/// 脚本/`-c`/stdin 的退出闭包：先跑 EXIT trap 再退出进程。
fn script_exit_fn(rc: i32) -> ! {
    run_exit_trap(&[], rc);
    std::process::exit(rc)
}

/// 执行脚本文件。
pub(crate) fn run_script_file(path: &str, args: Vec<String>) -> i32 {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("sh: {}: {}", path, e);
            return 127;
        }
    };
    params::set0(path);
    params::set(args);
    let rc = run_source(&content, &[], &|rc| script_exit_fn(rc), false);
    run_exit_trap(&[], rc);
    rc
}

/// 执行 `-c` 命令字符串。
pub(crate) fn run_command_string(cmd: &str, args: Vec<String>) -> i32 {
    // bash 语义：`sh -c 'cmd' name a b` 中 name 为 $0，其余为 $1...
    params::set0(args.first().map(String::as_str).unwrap_or("sh"));
    let positional = if args.len() > 1 {
        args[1..].to_vec()
    } else {
        Vec::new()
    };
    params::set(positional);
    let rc = run_source(cmd, &[], &|rc| script_exit_fn(rc), false);
    run_exit_trap(&[], rc);
    rc
}

/// 执行 stdin 脚本（无脚本文件时）。
pub(crate) fn run_stdin() -> i32 {
    use std::io::Read;
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        return 1;
    }
    let rc = run_source(&buf, &[], &|rc| script_exit_fn(rc), false);
    run_exit_trap(&[], rc);
    rc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::applets::core::shell::compound::tests::test_guard;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// 读取文件并过滤测试框架在重定向窗口内并发打印的行。
    fn read_filtered(path: &str) -> String {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.starts_with("test ") && !l.contains(" ... "))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn parse_args_modes() {
        options::reset_for_test();
        let a = parse_shell_args(&s(&["-c", "echo hi", "name", "a"])).unwrap();
        assert_eq!(a.command.as_deref(), Some("echo hi"));
        assert_eq!(a.args, vec!["name", "a"]);
        let a = parse_shell_args(&s(&["script.sh", "x"])).unwrap();
        assert_eq!(a.script.as_deref(), Some("script.sh"));
        assert_eq!(a.args, vec!["x"]);
        let a = parse_shell_args(&s(&["-ex", "script.sh"])).unwrap();
        assert!(options::errexit() && options::xtrace());
        assert_eq!(a.script.as_deref(), Some("script.sh"));
        let a = parse_shell_args(&s(&["-o", "pipefail", "s.sh"])).unwrap();
        assert!(options::pipefail());
        assert_eq!(a.script.as_deref(), Some("s.sh"));
        assert!(parse_shell_args(&s(&["-z"])).is_err());
        options::reset_for_test();
    }

    #[test]
    fn function_header_parsing() {
        assert_eq!(
            parse_function_header("greet() {"),
            Some(("greet".to_string(), String::new()))
        );
        assert_eq!(
            parse_function_header("function greet {"),
            Some(("greet".to_string(), String::new()))
        );
        assert_eq!(
            parse_function_header("greet() { echo hi; }"),
            Some(("greet".to_string(), " echo hi; }".to_string()))
        );
        assert!(parse_function_header("echo hi").is_none());
        assert!(parse_function_header("greet()").is_none());
    }

    #[test]
    fn collect_function_multiline() {
        let rest = s(&["  echo one", "  echo two", "}", "echo after"]);
        let (name, body, consumed, _) = collect_function("f() {", &rest).unwrap();
        assert_eq!(name, "f");
        assert!(body.contains("echo one") && body.contains("echo two"));
        assert_eq!(consumed, 3);
    }

    #[test]
    fn collect_function_inline() {
        let (name, body, consumed, tail) = collect_function("f() { echo hi; }", &[]).unwrap();
        assert!(tail.is_none());
        assert_eq!(name, "f");
        assert_eq!(body, "echo hi;");
        assert_eq!(consumed, 0);
    }

    #[test]
    fn heredoc_collection_and_expansion() {
        let rest = s(&["hello $USER", "EOF", "after"]);
        let (new_line, consumed) = collect_heredoc("cat <<EOF", &rest).unwrap();
        assert!(new_line.starts_with("cat < /tmp/rbox_heredoc_"));
        assert_eq!(consumed, 2);
        // 引号 delimiter：不展开（终止行不带引号）
        let rest = s(&["hello $USER", "EOF"]);
        let (new_line, _) = collect_heredoc("cat <<'EOF'", &rest).unwrap();
        let path = new_line.split_whitespace().last().unwrap();
        let content = std::fs::read_to_string(path).unwrap();
        assert!(content.contains("$USER"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn run_source_executes_commands() {
        let _g = test_guard();
        let path = format!("/tmp/rbox_script_test_{}", std::process::id());
        let _ = std::fs::remove_file(&path);
        let src = format!("x=1\nif true; then\n echo ran > {}\nfi\n", path);
        let rc = run_source(&src, &[], &|_| {}, false);
        assert_eq!(rc, 0);
        assert_eq!(read_filtered(&path).trim(), "ran");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn run_source_errexit_stops() {
        let _g = test_guard();
        options::reset_for_test();
        options::set_errexit(true);
        let path = format!("/tmp/rbox_errexit_{}", std::process::id());
        let _ = std::fs::remove_file(&path);
        let src = format!("false\necho no > {}\n", path);
        let rc = run_source(&src, &[], &|_| {}, false);
        assert_eq!(rc, 1);
        assert!(!std::path::Path::new(&path).exists());
        options::reset_for_test();
    }

    #[test]
    fn run_source_return_consumed() {
        let _g = test_guard();
        options::reset_for_test();
        let src = "return 7\necho never";
        let rc = run_source(src, &[], &|_| {}, false);
        assert_eq!(rc, 7);
        options::reset_for_test();
    }

    #[test]
    fn run_source_case_statement() {
        let _g = test_guard();
        let path = format!("/tmp/rbox_case_{}", std::process::id());
        let _ = std::fs::remove_file(&path);
        let src = format!(
            "x=b\ncase $x in\n a) echo A > {}\n ;;\n b) echo B > {}\n ;;\nesac\n",
            path, path
        );
        let rc = run_source(&src, &[], &|_| {}, false);
        assert_eq!(rc, 0);
        assert_eq!(read_filtered(&path).trim(), "B");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn run_source_until_loop() {
        let _g = test_guard();
        let path = format!("/tmp/rbox_until_{}", std::process::id());
        let _ = std::fs::remove_file(&path);
        // until false; do ... break; done
        let src = format!("until false\ndo\n echo u > {}\n break\ndone", path);
        let rc = run_source(&src, &[], &|_| {}, false);
        assert_eq!(rc, 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "u");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn run_source_subshell_isolates_state() {
        let _g = test_guard();
        let p = format!("/tmp/rbox_sub_{}", std::process::id());
        let _ = std::fs::remove_file(&p);
        let src = format!("x=1\n( x=2; echo $x > {p} )\necho $x >> {p}\n");
        let rc = run_source(&src, &[], &|_| {}, false);
        assert_eq!(rc, 0);
        assert_eq!(read_filtered(&p), "2\n1");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn run_source_group_redirect_and_negate() {
        let _g = test_guard();
        let p = format!("/tmp/rbox_grp_{}", std::process::id());
        let _ = std::fs::remove_file(&p);
        let src = format!("{{ echo g1; echo g2; }} > {p}\n! false\necho neg=$? >> {p}\n");
        let rc = run_source(&src, &[], &|_| {}, false);
        assert_eq!(rc, 0);
        assert_eq!(read_filtered(&p), "g1\ng2\nneg=0");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn run_source_compound_after_semicolon() {
        let _g = test_guard();
        let p = format!("/tmp/rbox_cmp2_{}", std::process::id());
        let _ = std::fs::remove_file(&p);
        let src = format!("set -- p q; for a; do echo $a >> {p}; done\n");
        let rc = run_source(&src, &[], &|_| {}, false);
        assert_eq!(rc, 0);
        assert_eq!(read_filtered(&p), "p\nq");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn run_source_compound_trailing_redirect_keeps_var() {
        let _g = test_guard();
        let p = format!("/tmp/rbox_done_{}", std::process::id());
        let f = format!("/tmp/rbox_done_in_{}", std::process::id());
        let _ = std::fs::remove_file(&p);
        std::fs::write(&f, "l1\nl2\n").unwrap();
        let src = format!("while read l; do c=$l; done < {f}\necho $c > {p}\n");
        let rc = run_source(&src, &[], &|_| {}, false);
        assert_eq!(rc, 0);
        assert_eq!(read_filtered(&p).trim(), "l2");
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn run_source_compound_after_semicolon_if_while() {
        let _g = test_guard();
        let p = format!("/tmp/rbox_cmp3_{}", std::process::id());
        let _ = std::fs::remove_file(&p);
        let src = format!(
            "true; if true; then echo IF >> {p}; fi\ni=0; while [ $i -lt 2 ]; do echo W$i >> {p}; i=$((i+1)); done\n"
        );
        let rc = run_source(&src, &[], &|_| {}, false);
        assert_eq!(rc, 0);
        assert_eq!(read_filtered(&p), "IF\nW0\nW1");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn run_source_operator_group() {
        let _g = test_guard();
        let p = format!("/tmp/rbox_opgrp_{}", std::process::id());
        let _ = std::fs::remove_file(&p);
        let src = format!(
            "true && ( echo AND >> {p} )\nfalse && ( echo BAD_AND >> {p} )\nfalse || ( echo OR >> {p} )\ntrue || ( echo BAD_OR >> {p} )\n"
        );
        let rc = run_source(&src, &[], &|_| {}, false);
        assert_eq!(rc, 0);
        assert_eq!(read_filtered(&p), "AND\nOR");
        let _ = std::fs::remove_file(&p);
    }
}
