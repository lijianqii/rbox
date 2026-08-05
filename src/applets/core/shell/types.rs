//! Shell 数据结构：Token、SimpleCmd、Pipeline、CommandList。

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
    /// `|` 管道。
    Pipe,
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
#[derive(Debug, Default, PartialEq)]
pub struct SimpleCmd {
    pub argv: Vec<String>,
    pub stdin_file: Option<String>,
    /// here-doc 内容（临时文件路径）。
    pub heredoc: Option<String>,
    pub stdout_file: Option<String>,
    pub stderr_file: Option<String>,
    pub append: bool,
    pub append_err: bool,
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
}
