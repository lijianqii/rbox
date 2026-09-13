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

/// 收集函数定义体：处理内联 `{ ... }` 与跨行 `{` ... `}`。
/// 返回 (函数名, 函数体文本, 额外消耗的行数)。
pub(crate) fn collect_function(
    line: &str,
    rest_lines: &[String],
) -> Option<(String, String, usize)> {
    let (name, after) = parse_function_header(line)?;
    let after = after.trim();
    // 内联：`{ cmd; }`
    if let Some(inner) = after.strip_suffix('}') {
        return Some((name, inner.trim().to_string(), 0));
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
            return Some((name, body, consumed));
        }
        if let Some(inner) = t.strip_suffix('}') {
            body.push_str(inner);
            body.push('\n');
            return Some((name, body, consumed));
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
        out.push_str(&super::expander::expand_vars(line, 0));
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
        // here-doc 收集
        if line.contains("<<")
            && let Some((new_line, consumed)) = collect_heredoc(&line, &lines[i..])
        {
            line = new_line;
            i += consumed;
        }
        // 函数定义
        if let Some((name, body, consumed)) = collect_function(&line, &lines[i..]) {
            functions::define(&name, &body);
            i += consumed;
            continue;
        }
        // 复合命令块
        if compound::is_compound_start(&line) {
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
            last_rc = compound::execute_block(&block.join("\n"), &mut last_rc, history, exit_fn);
        } else {
            last_rc = execute_line(&line, &mut last_rc, history, exit_fn);
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
        let (name, body, consumed) = collect_function("f() {", &rest).unwrap();
        assert_eq!(name, "f");
        assert!(body.contains("echo one") && body.contains("echo two"));
        assert_eq!(consumed, 3);
    }

    #[test]
    fn collect_function_inline() {
        let (name, body, consumed) = collect_function("f() { echo hi; }", &[]).unwrap();
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
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "ran");
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
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "B");
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
}
