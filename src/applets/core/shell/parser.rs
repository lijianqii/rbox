//! 命令列表解析器：将 Token 序列构建为 CommandList。

use super::types::*;

/// 将 Token 序列解析为 CommandList。
///
/// 语法：`pipeline (operator pipeline)*`
/// operator = `;` | `&&` | `||` | `&`
#[allow(unused_assignments)]
pub fn build_command_list(tokens: &[Token]) -> Result<CommandList, String> {
    let mut segments: Vec<LogicalSegment> = Vec::new();
    let mut cur_cmds: Vec<SimpleCmd> = Vec::new();
    let mut cur = SimpleCmd::default();
    let mut background = false;
    let mut connector = Connector::Start;
    let mut iter = tokens.iter().peekable();

    macro_rules! flush_pipeline {
        () => {{
            if !cur.is_empty() {
                cur_cmds.push(std::mem::take(&mut cur));
            }
            if !cur_cmds.is_empty() || background {
                segments.push(LogicalSegment {
                    pipeline: Pipeline {
                        cmds: std::mem::take(&mut cur_cmds),
                        background,
                    },
                    connector,
                });
                background = false;
                connector = Connector::Sequential;
            }
        }};
    }

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
                cur_cmds.push(std::mem::take(&mut cur));
            }
            Token::Semicolon => {
                flush_pipeline!();
                connector = Connector::Sequential;
            }
            Token::AndIf => {
                if cur.is_empty() && cur_cmds.is_empty() {
                    return Err("syntax error: empty command before &&".to_string());
                }
                flush_pipeline!();
                connector = Connector::AndIf;
            }
            Token::OrIf => {
                if cur.is_empty() && cur_cmds.is_empty() {
                    return Err("syntax error: empty command before ||".to_string());
                }
                flush_pipeline!();
                connector = Connector::OrIf;
            }
            Token::Background => {
                if cur.is_empty() && cur_cmds.is_empty() {
                    return Err("syntax error: empty command before &".to_string());
                }
                flush_pipeline!();
                connector = Connector::Sequential;
            }
        }
    }
    flush_pipeline!();

    Ok(CommandList { segments })
}

/// 从迭代器中取下一个 Word Token（用于重定向目标文件名）。
fn next_word<'a, I>(iter: &mut std::iter::Peekable<I>, op: &str) -> Result<String, String>
where
    I: Iterator<Item = &'a Token>,
{
    match iter.next() {
        Some(Token::Word(w)) => Ok(w.clone()),
        _ => Err(format!("syntax error: expected filename after {}", op)),
    }
}
