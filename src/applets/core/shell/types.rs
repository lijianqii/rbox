//! Shell 数据结构：Token、SimpleCmd、Pipeline、CommandList。

/// glob 保护标记（定义见共享工具 [`crate::applets::glob`]）。
pub(crate) use crate::applets::glob::GLOB_ESCAPE;

/// 双引号内 `$` 的标记：变量展开结果不做词分割（"$VAR" 语义）。
pub const NO_SPLIT_ESCAPE: char = '\u{2}';

/// 未加引号 `$` 的标记：变量展开结果参与词分割（$VAR 语义）。
pub const SPLIT_ESCAPE: char = '\u{3}';

/// 分词器产生的 Token。
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    /// 普通参数（已去除引号/转义）。
    Word(String),
    /// `>` 输出重定向（覆盖）。
    RedirOut,
    /// `>>` 输出重定向（追加）。
    RedirAppend,
    /// `<` 输入重定向。
    RedirIn,
    /// `<<` here-doc。
    RedirHereDoc,
    /// `2>` stderr 重定向（覆盖）。
    RedirErr,
    /// `2>>` stderr 重定向（追加）。
    RedirErrAppend,
    /// `N>&M` 文件描述符复制（from -> to）。
    RedirDup(u8, u8),
    /// `N>&-` 关闭文件描述符。
    RedirClose(u8),
    /// `&>` stdout+stderr 重定向（覆盖）。
    RedirOutBoth,
    /// `&>>` stdout+stderr 重定向（追加）。
    RedirOutBothAppend,
    /// `<<<` here-string。
    RedirHereString,
    /// `<>` 读写重定向。
    RedirInOut,
    /// `>|` 强制覆盖（绕过 noclobber）。
    RedirOutForce,
    /// `N>` / `N>>` 任意 fd 输出重定向。
    RedirFdOut(u8, bool),
    /// `N<` 任意 fd 输入重定向。
    RedirFdIn(u8),
    /// `|` 管道。
    Pipe,
    /// `|&` 管道（stdout+stderr 都进入管道）。
    PipeBoth,
    /// `;` 命令分隔。
    Semicolon,
    /// `&&` 条件与。
    AndIf,
    /// `||` 条件或。
    OrIf,
    /// `&` 后台运行。
    Background,
}

/// 一条简单命令（不含管道/重定向操作符，但持有重定向目标）。
#[derive(Debug, Default, PartialEq, Clone)]
pub struct SimpleCmd {
    pub argv: Vec<String>,
    pub stdin_file: Option<String>,
    /// here-doc 内容（临时文件路径）。
    pub heredoc: Option<String>,
    pub stdout_file: Option<String>,
    pub stderr_file: Option<String>,
    pub append: bool,
    pub append_err: bool,
    /// `N>&M` 描述符复制（按出现顺序应用，pre_exec 中 dup2）。
    pub dup_fds: Vec<(u8, u8)>,
    /// 命令级环境变量（`VAR=val cmd` 的前导赋值）。
    pub env: Vec<(String, String)>,
    /// 需要关闭的 fd（`N>&-`）。
    pub close_fds: Vec<u8>,
    /// here-string 内容（`<<< word`）。
    pub here_string: Option<String>,
    /// `|&`：stderr 也接入管道。
    pub stderr_to_pipe: bool,
    /// `<>` 读写文件。
    pub rw_file: Option<String>,
    /// `>|`：绕过 noclobber。
    pub force: bool,
    /// 任意 fd 重定向（`3>f`、`3<f` 等）。
    pub fd_redirects: Vec<FdRedirect>,
}

/// 任意 fd 重定向描述。
#[derive(Debug, Clone, PartialEq)]
pub struct FdRedirect {
    pub fd: u8,
    pub path: String,
    pub append: bool,
    pub input: bool,
}

impl SimpleCmd {
    /// 命令是否为空（无参数）。
    pub fn is_empty(&self) -> bool {
        self.argv.is_empty()
    }
}

/// 管道：一条或多条 SimpleCmd 串联。
#[derive(Debug, Default, PartialEq)]
pub struct Pipeline {
    pub cmds: Vec<SimpleCmd>,
    pub background: bool,
}

/// 逻辑连接符（`;` `&&` `||`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Connector {
    /// 命令列表的第一段。
    Start,
    /// `;` 顺序执行。
    Sequential,
    /// `&&` 前一条成功才执行。
    AndIf,
    /// `||` 前一条失败才执行。
    OrIf,
}

/// 一条逻辑段（一条 Pipeline + 连接符）。
#[derive(Debug, PartialEq)]
pub struct LogicalSegment {
    pub pipeline: Pipeline,
    pub connector: Connector,
}

/// 完整的命令列表（一行解析后的结果）。
#[derive(Debug, PartialEq)]
pub struct CommandList {
    pub segments: Vec<LogicalSegment>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_cmd_default() {
        let cmd = SimpleCmd::default();
        assert!(cmd.argv.is_empty());
        assert!(cmd.stdin_file.is_none());
        assert!(cmd.stdout_file.is_none());
        assert!(cmd.stderr_file.is_none());
        assert!(cmd.heredoc.is_none());
        assert!(!cmd.append);
        assert!(!cmd.append_err);
        assert!(cmd.dup_fds.is_empty());
        assert!(cmd.env.is_empty());
        assert!(cmd.close_fds.is_empty());
        assert!(cmd.here_string.is_none());
        assert!(!cmd.stderr_to_pipe);
        assert!(cmd.rw_file.is_none());
        assert!(!cmd.force);
        assert!(cmd.fd_redirects.is_empty());
    }

    #[test]
    fn simple_cmd_is_empty() {
        let cmd = SimpleCmd::default();
        assert!(cmd.is_empty());
    }

    #[test]
    fn simple_cmd_not_empty_with_argv() {
        let cmd = SimpleCmd {
            argv: vec!["echo".into()],
            ..Default::default()
        };
        assert!(!cmd.is_empty());
    }

    #[test]
    fn simple_cmd_not_empty_with_redirect_only() {
        let cmd = SimpleCmd {
            stdout_file: Some("/tmp/out".into()),
            ..Default::default()
        };
        // has redirect but no argv -> still "empty" (no command to run)
        assert!(cmd.is_empty());
    }

    #[test]
    fn pipeline_default() {
        let p = Pipeline::default();
        assert!(p.cmds.is_empty());
        assert!(!p.background);
    }

    #[test]
    fn command_list_default() {
        let cl = CommandList {
            segments: Vec::new(),
        };
        assert!(cl.segments.is_empty());
    }

    #[test]
    fn token_equality() {
        assert_eq!(Token::Pipe, Token::Pipe);
        assert_ne!(Token::Pipe, Token::Semicolon);
        assert_eq!(Token::Word("a".into()), Token::Word("a".into()));
        assert_ne!(Token::Word("a".into()), Token::Word("b".into()));
    }

    #[test]
    fn glob_escape_constant_is_control_char() {
        // 必须是不会出现在正常文本中的控制字符，避免与真实内容冲突
        assert!(GLOB_ESCAPE.is_control());
        assert_ne!(GLOB_ESCAPE, '\0');
    }
}
