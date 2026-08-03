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
    pub stdout_file: Option<String>,
    pub append: bool,
}

impl SimpleCmd {
    /// 命令是否为空（无参数）。
    pub fn is_empty(&self) -> bool {
        self.argv.is_empty()
    }
}

/// 管道：一条或多条 SimpleCmd 串联。
#[derive(Debug, PartialEq)]
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
