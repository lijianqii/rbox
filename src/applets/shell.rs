//! 基础 shell - 读取命令行、分词、fork+exec 传参。
//!
//! 功能范围：
//! - 读取一行输入，按空白分词，支持双引号保留空格、反斜杠转义。
//! - 输出重定向：`>` 覆盖、`>>` 追加。
//! - 输入重定向：`<`。
//! - 管道：`|`（多级）。
//! - 内置命令：`exit`、`cd`。
//! - 命令查找：先按字面路径，否则在 PATH 下查找；再回退到 rbox 内置 applet。

use crate::applet::Applet;
use std::io::{self, BufRead, Write};
use std::process::{Child, Command, ExitCode, Stdio};

pub struct Shell;
pub static SHELL: &Shell = &Shell;

/// 分词后的 token：要么是普通单词，要么是操作符。
#[derive(Debug, Clone)]
enum Token {
    Word(String),
    /// > 覆盖输出
    RedirOut,
    /// >> 追加输出
    RedirAppend,
    /// < 输入
    RedirIn,
    /// | 管道
    Pipe,
}

/// 一条简单命令（不含管道，但含重定向）。
#[derive(Debug, Default)]
struct SimpleCmd {
    argv: Vec<String>,
    stdin_file: Option<String>,
    stdout_file: Option<String>,
    append: bool,
}

impl SimpleCmd {
    fn is_empty(&self) -> bool {
        self.argv.is_empty()
    }
}

/// 一条管线：由 `|` 连接的若干 SimpleCmd。
struct Pipeline {
    cmds: Vec<SimpleCmd>,
}

impl Applet for Shell {
    fn name(&self) -> &'static str {
        "shell"
    }
    fn help(&self) -> &'static str {
        "shell - command interpreter (pipes, redirections)"
    }
    fn run(&self, _args: &[String]) -> ExitCode {
        if std::env::var_os("PATH").is_none() {
            // SAFETY: shell 启动时单线程，无并发风险
            unsafe { std::env::set_var("PATH", "/bin:/sbin:/usr/bin:/usr/sbin") };
        }

        let stdin = io::stdin();
        let mut lines = stdin.lock().lines();
        let mut last_rc: i32 = 0;

        let _ = write!(io::stdout(), "rbox# ");
        let _ = io::stdout().flush();

        while let Some(line) = lines.next() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };

            let tokens = tokenize(&line);
            // 内置命令（仅当无管道/重定向时直接处理）
            if let Some(rc) = try_builtin(&tokens, &mut last_rc) {
                if rc.is_exit() {
                    return rc.into_exit_code();
                }
                let _ = write!(io::stdout(), "rbox# ");
                let _ = io::stdout().flush();
                continue;
            }

            let pipeline = match build_pipeline(&tokens) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("shell: {}", e);
                    last_rc = 1;
                    let _ = write!(io::stdout(), "rbox# ");
                    let _ = io::stdout().flush();
                    continue;
                }
            };

            if pipeline.cmds.is_empty() {
                let _ = write!(io::stdout(), "rbox# ");
                let _ = io::stdout().flush();
                continue;
            }

            last_rc = execute_pipeline(&pipeline);
            let _ = write!(io::stdout(), "rbox# ");
            let _ = io::stdout().flush();
        }

        ExitCode::from(last_rc as u8)
    }
}

// ─── 分词 ─────────────────────────────────────────────

/// 将一行切分为 token 序列。操作符 > >> < | 作为独立 token。
/// 引号内不识别操作符。
fn tokenize(line: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut in_token = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    cur.push(next);
                    in_token = true;
                }
            }
            '"' => {
                in_quote = !in_quote;
                in_token = true;
            }
            '>' if !in_quote => {
                if in_token {
                    tokens.push(Token::Word(std::mem::take(&mut cur)));
                    in_token = false;
                }
                if chars.peek() == Some(&'>') {
                    chars.next();
                    tokens.push(Token::RedirAppend);
                } else {
                    tokens.push(Token::RedirOut);
                }
            }
            '<' if !in_quote => {
                if in_token {
                    tokens.push(Token::Word(std::mem::take(&mut cur)));
                    in_token = false;
                }
                tokens.push(Token::RedirIn);
            }
            '|' if !in_quote => {
                if in_token {
                    tokens.push(Token::Word(std::mem::take(&mut cur)));
                    in_token = false;
                }
                tokens.push(Token::Pipe);
            }
            ' ' | '\t' if !in_quote => {
                if in_token {
                    tokens.push(Token::Word(std::mem::take(&mut cur)));
                    in_token = false;
                }
            }
            _ => {
                cur.push(c);
                in_token = true;
            }
        }
    }
    if in_token {
        tokens.push(Token::Word(cur));
    }
    tokens
}

// ─── 内置命令 ─────────────────────────────────────────

/// 内置命令的返回值：要么继续、要么退出。
enum BuiltinResult {
    Continue,
    Exit(ExitCode),
}

impl BuiltinResult {
    fn is_exit(&self) -> bool {
        matches!(self, BuiltinResult::Exit(_))
    }
    fn into_exit_code(self) -> ExitCode {
        match self {
            BuiltinResult::Exit(c) => c,
            BuiltinResult::Continue => ExitCode::SUCCESS,
        }
    }
}

/// 仅当整行不含管道/重定向操作符时，才尝试内置命令。
fn try_builtin(tokens: &[Token], last_rc: &mut i32) -> Option<BuiltinResult> {
    if tokens.iter().any(|t| !matches!(t, Token::Word(_))) {
        return None;
    }
    let words: Vec<&str> = tokens
        .iter()
        .filter_map(|t| match t {
            Token::Word(w) => Some(w.as_str()),
            _ => None,
        })
        .collect();
    if words.is_empty() {
        return None;
    }
    let cmd = words[0];

    match cmd {
        "exit" => {
            let code = words
                .get(1)
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(*last_rc);
            Some(BuiltinResult::Exit(ExitCode::from(code as u8)))
        }
        "cd" => {
            let target = words.get(1).copied().unwrap_or("/");
            if let Err(e) = std::env::set_current_dir(target) {
                eprintln!("cd: {}", e);
                *last_rc = 1;
            } else {
                *last_rc = 0;
            }
            Some(BuiltinResult::Continue)
        }
        _ => None,
    }
}

// ─── 管线构建 ─────────────────────────────────────────

/// 把 token 序列构建为 Pipeline。
fn build_pipeline(tokens: &[Token]) -> Result<Pipeline, String> {
    let mut cmds: Vec<SimpleCmd> = Vec::new();
    let mut cur = SimpleCmd::default();

    let mut iter = tokens.iter().peekable();
    while let Some(tok) = iter.next() {
        match tok {
            Token::Word(w) => cur.argv.push(w.clone()),
            Token::RedirOut => {
                let f = next_word(&mut iter, ">")?;
                cur.stdout_file = Some(f);
                cur.append = false;
            }
            Token::RedirAppend => {
                let f = next_word(&mut iter, ">>")?;
                cur.stdout_file = Some(f);
                cur.append = true;
            }
            Token::RedirIn => {
                let f = next_word(&mut iter, "<")?;
                cur.stdin_file = Some(f);
            }
            Token::Pipe => {
                if cur.is_empty() {
                    return Err("syntax error: empty command before |".to_string());
                }
                cmds.push(std::mem::take(&mut cur));
            }
        }
    }
    if cur.is_empty() && !cmds.is_empty() {
        return Err("syntax error: empty command after |".to_string());
    }
    if !cur.is_empty() {
        cmds.push(cur);
    }

    Ok(Pipeline { cmds })
}

/// 从 peekable 迭代器取下一个 Word token，作为重定向的目标文件名。
fn next_word<'a, I>(iter: &mut std::iter::Peekable<I>, op: &str) -> Result<String, String>
where
    I: Iterator<Item = &'a Token>,
{
    match iter.next() {
        Some(Token::Word(w)) => Ok(w.clone()),
        _ => Err(format!("syntax error: expected filename after {}", op)),
    }
}

// ─── 管线执行 ─────────────────────────────────────────

/// 执行一条管线，返回最后一条命令的退出码。
fn execute_pipeline(pipeline: &Pipeline) -> i32 {
    let n = pipeline.cmds.len();
    if n == 0 {
        return 0;
    }

    // 单命令无管道
    if n == 1 {
        return run_simple(&pipeline.cmds[0], None, None);
    }

    // 多命令管道：逐个创建，用 pipe 串联
    let mut prev_stdout: Option<std::process::ChildStdout> = None;
    let mut children: Vec<Child> = Vec::new();

    for (i, cmd) in pipeline.cmds.iter().enumerate() {
        let is_first = i == 0;
        let is_last = i == n - 1;

        let stdin = if is_first {
            // 第一条：可能从文件重定向输入
            if let Some(f) = &cmd.stdin_file {
                match std::fs::File::open(f) {
                    Ok(file) => Stdio::from(file).into(),
                    Err(e) => {
                        eprintln!("shell: {}: {}", f, e);
                        return 1;
                    }
                }
            } else {
                Stdio::inherit()
            }
        } else {
            // 中间命令：从上一条的 stdout 读
            prev_stdout
                .take()
                .map(|s| Stdio::from(s))
                .unwrap_or_else(Stdio::inherit)
        };

        let stdout = if is_last {
            // 最后一条：可能重定向到文件
            if let Some(f) = &cmd.stdout_file {
                match open_stdout(f, cmd.append) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("shell: {}: {}", f, e);
                        // 清理已启动的管道子进程，避免遗留
                        for mut c in children.drain(..) {
                            let _ = c.kill();
                            let _ = c.wait();
                        }
                        return 1;
                    }
                }
            } else {
                Stdio::inherit()
            }
        } else {
            // 中间命令：创建管道供下一条读
            Stdio::piped()
        };

        let child = spawn_command(cmd, stdin, stdout);
        match child {
            Ok(mut c) => {
                prev_stdout = c.stdout.take();
                children.push(c);
            }
            Err(e) => {
                eprintln!("shell: {}", e);
                // 清理已启动的管道子进程，避免遗留
                for mut c in children.drain(..) {
                    let _ = c.kill();
                    let _ = c.wait();
                }
                return 127;
            }
        }
    }

    // 等待所有子进程
    let mut last_rc = 0;
    for mut c in children {
        match c.wait() {
            Ok(status) => last_rc = status.code().unwrap_or(1),
            Err(_) => last_rc = 1,
        }
    }
    last_rc
}

/// 执行单条命令（无管道，但可能含重定向）。
fn run_simple(cmd: &SimpleCmd, stdin: Option<Stdio>, stdout: Option<Stdio>) -> i32 {
    let mut command = build_command(&cmd.argv);

    // 输入重定向：调用参数优先，其次命令行中的 < file；打开失败直接报错返回
    if let Some(s) = stdin {
        command.stdin(s);
    } else if let Some(f) = &cmd.stdin_file {
        match std::fs::File::open(f) {
            Ok(file) => {
                command.stdin(Stdio::from(file));
            }
            Err(e) => {
                eprintln!("shell: {}: {}", f, e);
                return 1;
            }
        }
    }

    // 输出重定向：调用参数优先，其次命令行中的 > file / >> file；打开失败直接报错返回
    if let Some(o) = stdout {
        command.stdout(o);
    } else if let Some(f) = &cmd.stdout_file {
        match open_stdout(f, cmd.append) {
            Ok(s) => {
                command.stdout(s);
            }
            Err(e) => {
                eprintln!("shell: {}: {}", f, e);
                return 1;
            }
        }
    }

    match command.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("{}: {}", cmd.argv[0], e);
            127
        }
    }
}

/// 根据 argv 创建 Command：先 PATH 查找，再回退 rbox 内置 applet。
fn build_command(argv: &[String]) -> Command {
    let cmd_name = &argv[0];
    if let Some(path) = resolve_command(cmd_name) {
        let mut c = Command::new(path);
        c.args(&argv[1..]);
        return c;
    }
    // 回退：rbox 内置 applet
    let is_builtin = crate::applet::APPLETS.iter().any(|a| a.name() == cmd_name);
    if is_builtin {
        let mut c = Command::new("/bin/rbox");
        c.arg(cmd_name);
        c.args(&argv[1..]);
        return c;
    }
    // 不存在：仍创建（spawn 时会报错）
    let mut c = Command::new(cmd_name);
    c.args(&argv[1..]);
    c
}

/// spawn 一条命令（用于管道中间步骤）。
fn spawn_command(cmd: &SimpleCmd, stdin: Stdio, stdout: Stdio) -> io::Result<Child> {
    let mut command = build_command(&cmd.argv);
    command.stdin(stdin);
    command.stdout(stdout);
    // 管道中间命令不支持单独的重定向（简化）
    command.spawn()
}

/// 打开 stdout 文件用于重定向。失败时返回错误（由调用方报错并返回非零）。
fn open_stdout(path: &str, append: bool) -> io::Result<Stdio> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true);
    if append {
        opts.append(true);
    } else {
        opts.truncate(true);
    }
    opts.open(path).map(Stdio::from)
}

/// 解析命令路径：含 / 则按字面；否则在 PATH 各目录下查找可执行文件。
fn resolve_command(cmd: &str) -> Option<std::path::PathBuf> {
    if cmd.contains('/') {
        let p = std::path::PathBuf::from(cmd);
        if p.is_file() {
            return Some(p);
        }
        return None;
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 将 token 序列展平为字符串，便于断言。
    fn flatten(tokens: &[Token]) -> Vec<String> {
        tokens
            .iter()
            .map(|t| match t {
                Token::Word(w) => w.clone(),
                Token::RedirOut => ">".to_string(),
                Token::RedirAppend => ">>".to_string(),
                Token::RedirIn => "<".to_string(),
                Token::Pipe => "|".to_string(),
            })
            .collect()
    }

    #[test]
    fn tokenize_simple_words() {
        assert_eq!(flatten(&tokenize("echo hello world")), ["echo", "hello", "world"]);
    }

    #[test]
    fn tokenize_redirections() {
        assert_eq!(flatten(&tokenize("cat a > b")), ["cat", "a", ">", "b"]);
        assert_eq!(flatten(&tokenize("cat a >> b")), ["cat", "a", ">>", "b"]);
        assert_eq!(flatten(&tokenize("cat < in")), ["cat", "<", "in"]);
    }

    #[test]
    fn tokenize_quote_keeps_spaces() {
        assert_eq!(flatten(&tokenize("echo \"hello world\"")), ["echo", "hello world"]);
    }

    #[test]
    fn tokenize_backslash_escape() {
        assert_eq!(flatten(&tokenize("echo hello\\ world")), ["echo", "hello world"]);
    }

    #[test]
    fn tokenize_pipe_without_spaces() {
        assert_eq!(flatten(&tokenize("a|b")), ["a", "|", "b"]);
    }

    #[test]
    fn tokenize_operator_not_recognized_in_quote() {
        assert_eq!(flatten(&tokenize("echo \"a|b\"")), ["echo", "a|b"]);
    }

    #[test]
    fn build_pipeline_basic() {
        let p = build_pipeline(&tokenize("echo hi > f")).unwrap();
        assert_eq!(p.cmds.len(), 1);
        assert_eq!(p.cmds[0].argv, ["echo", "hi"]);
        assert_eq!(p.cmds[0].stdout_file.as_deref(), Some("f"));
        assert!(!p.cmds[0].append);
    }

    #[test]
    fn build_pipeline_append_and_input() {
        let p = build_pipeline(&tokenize("cat < in | grep x >> out")).unwrap();
        assert_eq!(p.cmds.len(), 2);
        assert_eq!(p.cmds[0].stdin_file.as_deref(), Some("in"));
        assert_eq!(p.cmds[1].stdout_file.as_deref(), Some("out"));
        assert!(p.cmds[1].append);
    }

    #[test]
    fn build_pipeline_syntax_errors() {
        assert!(build_pipeline(&tokenize("cat |")).is_err());
        assert!(build_pipeline(&tokenize("| cat")).is_err());
        assert!(build_pipeline(&tokenize("cat >")).is_err());
    }

    #[test]
    fn build_pipeline_empty_line() {
        let p = build_pipeline(&tokenize("")).unwrap();
        assert!(p.cmds.is_empty());
    }

    #[test]
    fn open_stdout_failure_is_reported() {
        // 父目录不存在，打开必然失败，错误必须向上传播而非静默
        assert!(open_stdout("/nonexistent-rbox-dir/out", false).is_err());
        assert!(open_stdout("/nonexistent-rbox-dir/out", true).is_err());
    }
}
