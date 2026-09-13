//! 复合命令：`if`/`elif`/`else`/`fi`、`for ... in ... do ... done`、
//! `while ... do ... done`。
//!
//! REPL 逐行累积复合命令（`shell::run_shell` 用 [`nesting_delta`] 判断是否
//! 输入完整），完整后交给 [`execute_block`] 解析执行。块内每行通过
//! `executor::execute_line` 执行，天然支持嵌套复合命令。
//!
//! 规范化：先按引号外的 `;` 拆分（`if a; then` -> `if a` + `then`），
//! 因此 `then`/`do`/`fi`/`done` 均可与前置命令同行或独占一行。

use super::executor::execute_line;
use super::expander::expand_word;

/// 内部退出码哨兵：`break` / `continue`（在 exec_lines 中拦截，
/// 由循环执行器消费；顶层出现时按无操作处理）。
const RC_BREAK: i32 = 200;
const RC_CONTINUE: i32 = 201;

/// 行首关键字是否开启复合命令。
pub fn is_compound_start(line: &str) -> bool {
    matches!(
        first_word_of_first_part(line).as_str(),
        "if" | "for" | "while"
    )
}

/// 计算一行的嵌套深度增量：命令位置出现 if/for/while 为 +1，fi/done 为 -1。
pub fn nesting_delta(line: &str) -> i32 {
    let mut delta = 0;
    for part in split_semicolons(line) {
        match first_word(&part) {
            "if" | "for" | "while" => delta += 1,
            "fi" | "done" => delta -= 1,
            _ => {}
        }
    }
    delta
}

/// 按引号外的 `;` 拆分一行（引号内的分号保留）。
fn split_semicolons(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    let mut in_squote = false;
    let mut in_dquote = false;
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
                let ch = line[i..].chars().next().unwrap_or('\u{fffd}');
                cur.push(ch);
                i += ch.len_utf8();
                let ch2 = line[i..].chars().next().unwrap_or('\u{fffd}');
                cur.push(ch2);
                i += ch2.len_utf8();
                continue;
            }
            let ch = line[i..].chars().next().unwrap_or('\u{fffd}');
            cur.push(ch);
            i += ch.len_utf8();
            if c == b'"' {
                in_dquote = false;
            }
            continue;
        }
        match c {
            b'\'' => {
                in_squote = true;
                cur.push('\'');
                i += 1;
            }
            b'"' => {
                in_dquote = true;
                cur.push('"');
                i += 1;
            }
            b';' => {
                parts.push(std::mem::take(&mut cur));
                i += 1;
            }
            _ => {
                let ch = line[i..].chars().next().unwrap_or('\u{fffd}');
                cur.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    parts.push(cur);
    parts
}

/// 取一个命令段的首词。
fn first_word(part: &str) -> &str {
    part.split_whitespace().next().unwrap_or("")
}

/// 取第一个 `;` 段的首词（判断行首命令）。
fn first_word_of_first_part(line: &str) -> String {
    split_semicolons(line)
        .first()
        .map(|p| first_word(p).to_string())
        .unwrap_or_default()
}

/// 去除行首关键字，返回其余部分（关键字必须是独立词）。
fn strip_keyword<'a>(line: &'a str, kw: &str) -> Option<&'a str> {
    let t = line.trim();
    if t == kw {
        return Some("");
    }
    let rest = t.strip_prefix(kw)?;
    if rest.starts_with(char::is_whitespace) {
        Some(rest.trim_start())
    } else {
        None
    }
}

/// 规范化块：拆分引号外分号，并把行首的 then/do/else/fi/done 关键字
/// 拆成独立行（支持 `if a; then b; fi` 单行写法），去空行/首尾空白。
pub(crate) fn normalize(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in source.lines() {
        for part in split_semicolons(line) {
            let t = part.trim();
            if t.is_empty() {
                continue;
            }
            // 行首关键字独立成行："then echo x" -> "then" + "echo x"
            let mut split = false;
            for kw in ["then", "do", "else", "fi", "done"] {
                if let Some(rest) = strip_keyword(t, kw) {
                    out.push(kw.to_string());
                    if !rest.is_empty() {
                        out.push(rest.to_string());
                    }
                    split = true;
                    break;
                }
            }
            if !split {
                out.push(t.to_string());
            }
        }
    }
    out
}

/// 从 start（该行为复合命令起始）出发找匹配的结束行下标。
/// 用期望终结符栈处理嵌套（if->fi，for/while->done）。
fn find_block_end(lines: &[String], start: usize, terminator: &str) -> Option<usize> {
    let mut stack: Vec<&str> = vec![terminator];
    let mut i = start + 1;
    while i < lines.len() {
        let l = lines[i].as_str();
        if is_compound_start(l) {
            stack.push(if first_word_of_first_part(l) == "if" {
                "fi"
            } else {
                "done"
            });
        } else if l == "fi" || l == "done" {
            let expected = *stack.last()?;
            if l == expected {
                stack.pop();
                if stack.is_empty() {
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

/// 执行块内多行（递归处理嵌套复合命令）。
fn exec_lines(
    lines: &[String],
    last_rc: &mut i32,
    history: &[String],
    exit_fn: &dyn Fn(i32),
) -> i32 {
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].clone();
        // break / continue：返回哨兵由循环消费
        match line.as_str() {
            "break" => {
                *last_rc = 0;
                return RC_BREAK;
            }
            "continue" => {
                *last_rc = 0;
                return RC_CONTINUE;
            }
            _ => {}
        }
        if is_compound_start(&line) {
            let kw = first_word_of_first_part(&line);
            let terminator = if kw == "if" { "fi" } else { "done" };
            let Some(end) = find_block_end(lines, i, terminator) else {
                eprintln!("shell: syntax error: missing '{}'", terminator);
                *last_rc = 2;
                return 2;
            };
            let slice = &lines[i..=end];
            let rc = match kw.as_str() {
                "if" => exec_if(slice, last_rc, history, exit_fn),
                "for" => exec_for(slice, last_rc, history, exit_fn),
                "while" => exec_while(slice, last_rc, history, exit_fn),
                _ => 0,
            };
            // 哨兵（break/continue）向上传播；普通退出码写入 last_rc
            if rc == RC_BREAK || rc == RC_CONTINUE {
                return rc;
            }
            *last_rc = rc;
            i = end + 1;
        } else {
            *last_rc = execute_line(&line, last_rc, history, exit_fn);
            if *last_rc == 130 {
                // Ctrl-C 中断：终止块执行，与 bash 行为一致
                return 130;
            }
            i += 1;
        }
    }
    *last_rc
}

/// 执行 if 块（slice 以 if 行开始、fi 行结束）。返回最后一个执行命令的退出码
/// 或 break/continue 哨兵。
fn exec_if(slice: &[String], last_rc: &mut i32, history: &[String], exit_fn: &dyn Fn(i32)) -> i32 {
    let mut i = 1; // 跳过 "if ..."
    let mut cond = strip_keyword(&slice[0], "if").unwrap_or("").to_string();
    // 条件可跨行，直到 then
    while i < slice.len() && slice[i] != "then" {
        cond.push_str("; ");
        cond.push_str(&slice[i]);
        i += 1;
    }
    if i >= slice.len() {
        eprintln!("shell: syntax error: missing 'then'");
        *last_rc = 2;
        return 2;
    }
    if cond.trim().is_empty() {
        eprintln!("shell: syntax error: missing condition after 'if'");
        *last_rc = 2;
        return 2;
    }
    i += 1; // 跳过 then
    let mut branches: Vec<(String, Vec<String>)> = Vec::new();
    let mut else_body: Vec<String> = Vec::new();
    let mut body: Vec<String> = Vec::new();
    let mut in_else = false;
    while i < slice.len() {
        let line = &slice[i];
        if line == "fi" {
            break;
        }
        if let Some(rest) = strip_keyword(line, "elif") {
            if in_else {
                eprintln!("shell: syntax error: 'elif' after 'else'");
                *last_rc = 2;
                return 2;
            }
            branches.push((std::mem::take(&mut cond), std::mem::take(&mut body)));
            cond = rest.to_string();
            i += 1;
            while i < slice.len() && slice[i] != "then" {
                cond.push_str("; ");
                cond.push_str(&slice[i]);
                i += 1;
            }
            if i >= slice.len() {
                eprintln!("shell: syntax error: missing 'then' after 'elif'");
                *last_rc = 2;
                return 2;
            }
        } else if line == "else" {
            if in_else {
                eprintln!("shell: syntax error: duplicated 'else'");
                *last_rc = 2;
                return 2;
            }
            branches.push((std::mem::take(&mut cond), std::mem::take(&mut body)));
            in_else = true;
        } else {
            body.push(line.clone());
        }
        i += 1;
    }
    if in_else {
        else_body = body;
    } else if !cond.is_empty() {
        branches.push((cond, body));
    }

    for (c, b) in &branches {
        *last_rc = execute_line(c, last_rc, history, exit_fn);
        if *last_rc == 0 {
            let rc = exec_lines(b, last_rc, history, exit_fn);
            if rc == RC_BREAK || rc == RC_CONTINUE {
                return rc;
            }
            return *last_rc;
        }
    }
    if !else_body.is_empty() {
        let rc = exec_lines(&else_body, last_rc, history, exit_fn);
        if rc == RC_BREAK || rc == RC_CONTINUE {
            return rc;
        }
        return *last_rc;
    }
    *last_rc = 0;
    0
}

/// 执行 for 块（slice 以 for 行开始、done 行结束）。
fn exec_for(slice: &[String], last_rc: &mut i32, history: &[String], exit_fn: &dyn Fn(i32)) -> i32 {
    let header = strip_keyword(&slice[0], "for").unwrap_or("");
    let (name, words_part) = match header.split_once(char::is_whitespace) {
        Some((n, rest)) => {
            let rest = rest.trim();
            let words = rest.strip_prefix("in").map(str::trim).unwrap_or("");
            (n.to_string(), words.to_string())
        }
        None => (header.to_string(), String::new()),
    };
    if name.is_empty() {
        eprintln!("shell: syntax error: for requires a variable name");
        *last_rc = 2;
        return 2;
    }
    // 跳过 "do"
    let mut i = 1;
    while i < slice.len() && slice[i] != "do" {
        i += 1;
    }
    if i >= slice.len() {
        eprintln!("shell: syntax error: missing 'do'");
        *last_rc = 2;
        return 2;
    }
    i += 1;
    let body: Vec<String> = slice[i..]
        .iter()
        .take_while(|l| l.as_str() != "done")
        .cloned()
        .collect();

    let words: Vec<String> = words_part
        .split_whitespace()
        .flat_map(|w| expand_word(w, *last_rc))
        .collect();
    if words.is_empty() {
        *last_rc = 0;
        return 0;
    }
    for w in words {
        // SAFETY: shell 单线程执行，无并发环境变量访问
        unsafe {
            std::env::set_var(&name, &w);
        }
        let rc = exec_lines(&body, last_rc, history, exit_fn);
        if rc == RC_BREAK {
            break;
        }
        if rc == 130 {
            return 130;
        }
        // continue 哨兵：继续下一轮
    }
    *last_rc
}

/// 执行 while 块（slice 以 while 行开始、done 行结束）。
fn exec_while(
    slice: &[String],
    last_rc: &mut i32,
    history: &[String],
    exit_fn: &dyn Fn(i32),
) -> i32 {
    let mut i = 1;
    let mut cond = strip_keyword(&slice[0], "while").unwrap_or("").to_string();
    while i < slice.len() && slice[i] != "do" {
        cond.push_str("; ");
        cond.push_str(&slice[i]);
        i += 1;
    }
    if i >= slice.len() {
        eprintln!("shell: syntax error: missing 'do'");
        *last_rc = 2;
        return 2;
    }
    if cond.trim().is_empty() {
        eprintln!("shell: syntax error: missing condition after 'while'");
        *last_rc = 2;
        return 2;
    }
    i += 1; // 跳过 do
    let body: Vec<String> = slice[i..]
        .iter()
        .take_while(|l| l.as_str() != "done")
        .cloned()
        .collect();

    loop {
        *last_rc = execute_line(&cond, last_rc, history, exit_fn);
        if *last_rc != 0 {
            *last_rc = 0;
            return 0;
        }
        let rc = exec_lines(&body, last_rc, history, exit_fn);
        if rc == RC_BREAK {
            break;
        }
        if rc == 130 {
            return 130;
        }
        // continue 哨兵：继续下一轮
    }
    *last_rc
}

/// 执行一段复合命令文本（REPL 累积的完整块）。
pub fn execute_block(
    source: &str,
    last_rc: &mut i32,
    history: &[String],
    exit_fn: &dyn Fn(i32),
) -> i32 {
    let lines = normalize(source);
    if lines.is_empty() {
        return *last_rc;
    }
    let rc = exec_lines(&lines, last_rc, history, exit_fn);
    // 顶层 break/continue：仅警告并按无操作处理（与 bash 一致）
    if rc == RC_BREAK {
        eprintln!("shell: break: only meaningful in a for/while loop");
        *last_rc = 0;
    } else if rc == RC_CONTINUE {
        eprintln!("shell: continue: only meaningful in a for/while loop");
        *last_rc = 0;
    }
    *last_rc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_exit(_rc: i32) {}

    #[test]
    fn detects_compound_start() {
        assert!(is_compound_start("if true"));
        assert!(is_compound_start("for i in a b"));
        assert!(is_compound_start("while false"));
        assert!(is_compound_start("if true; then echo x; fi"));
        assert!(!is_compound_start("echo if"));
        assert!(!is_compound_start("iffy"));
    }

    #[test]
    fn nesting_delta_counts_keywords() {
        assert_eq!(nesting_delta("if true; then"), 1);
        assert_eq!(nesting_delta("fi"), -1);
        assert_eq!(nesting_delta("for i in a; do"), 1);
        assert_eq!(nesting_delta("done"), -1);
        assert_eq!(nesting_delta("echo if fi"), 0);
        assert_eq!(nesting_delta("if true; then echo a; fi"), 0);
    }

    #[test]
    fn normalize_splits_semicolons() {
        assert_eq!(
            normalize("if true; then\n echo a\necho b; fi"),
            vec!["if true", "then", "echo a", "echo b", "fi"]
        );
        // 引号内分号保留
        assert_eq!(normalize("echo 'a;b'"), vec!["echo 'a;b'"]);
    }

    #[test]
    fn normalize_splits_inline_keywords() {
        assert_eq!(
            normalize("if true; then echo a; fi"),
            vec!["if true", "then", "echo a", "fi"]
        );
        assert_eq!(
            normalize("for x in 1 2; do echo $x; done"),
            vec!["for x in 1 2", "do", "echo $x", "done"]
        );
        assert_eq!(
            normalize("if false; then a; else b; fi"),
            vec!["if false", "then", "a", "else", "b", "fi"]
        );
        // "done" 不应被误拆为 "do" + "ne"
        assert_eq!(normalize("done"), vec!["done"]);
    }

    #[test]
    fn execute_if_taken_branch() {
        let path = format!("/tmp/rbox_if_taken_{}", std::process::id());
        let _ = std::fs::remove_file(&path);
        let src = format!("if true; then\n echo taken_branch > {}\nfi", path);
        let mut rc = 0;
        execute_block(&src, &mut rc, &[], &noop_exit);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            "taken_branch"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn execute_if_else_branch() {
        // 条件失败走 else：用 echo 写入临时文件验证分支选择
        let path = format!("/tmp/rbox_if_else_{}", std::process::id());
        let _ = std::fs::remove_file(&path);
        let src = format!(
            "if false; then\n echo then_branch > {}\nelse\n echo else_branch > {}\nfi",
            path, path
        );
        let mut rc = 0;
        execute_block(&src, &mut rc, &[], &noop_exit);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            "else_branch"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn execute_for_iterates_words() {
        let path = format!("/tmp/rbox_for_{}", std::process::id());
        let _ = std::fs::remove_file(&path);
        let src = format!("for i in aa bb cc\ndo\n echo $i >> {}\ndone", path);
        let mut rc = 0;
        execute_block(&src, &mut rc, &[], &noop_exit);
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "aa\nbb\ncc\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn execute_while_with_break_terminates() {
        let path = format!("/tmp/rbox_while_{}", std::process::id());
        let _ = std::fs::remove_file(&path);
        let src = format!("while true\ndo\n echo x >> {}\n break\ndone", path);
        let mut rc = 0;
        execute_block(&src, &mut rc, &[], &noop_exit);
        assert_eq!(rc, 0);
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "x\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn for_continue_skips_iteration() {
        let path = format!("/tmp/rbox_continue_{}", std::process::id());
        let _ = std::fs::remove_file(&path);
        let src = format!(
            "for i in aa bb\ndo\n if true; then\n  continue\n fi\n echo $i >> {}\ndone",
            path
        );
        let mut rc = 0;
        execute_block(&src, &mut rc, &[], &noop_exit);
        // continue 跳过 echo：文件应不存在或为空
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(content, "");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_terminator_reports_error() {
        let mut rc = 0;
        execute_block("if true; then", &mut rc, &[], &noop_exit);
        assert_eq!(rc, 2);
    }

    #[test]
    fn missing_then_reports_error() {
        let mut rc = 0;
        execute_block("if true\nfi", &mut rc, &[], &noop_exit);
        assert_eq!(rc, 2);
    }

    #[test]
    fn elif_after_else_reports_error() {
        let mut rc = 0;
        execute_block(
            "if false; then\na\nelse\nb\nelif true; then\nc\nfi",
            &mut rc,
            &[],
            &noop_exit,
        );
        assert_eq!(rc, 2);
    }

    #[test]
    fn missing_do_reports_error() {
        let mut rc = 0;
        execute_block("for i in a b\ndone", &mut rc, &[], &noop_exit);
        assert_eq!(rc, 2);
        let mut rc = 0;
        execute_block("while true\ndone", &mut rc, &[], &noop_exit);
        assert_eq!(rc, 2);
    }

    #[test]
    fn empty_condition_reports_error() {
        let mut rc = 0;
        execute_block("if\nthen\necho x\nfi", &mut rc, &[], &noop_exit);
        assert_eq!(rc, 2);
        let mut rc = 0;
        execute_block("while\ndo\necho x\ndone", &mut rc, &[], &noop_exit);
        assert_eq!(rc, 2);
    }
}
