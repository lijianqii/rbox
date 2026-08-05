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
            Token::RedirHereDoc => {
                let delim = next_word(&mut iter, "<<")?;
                cur.heredoc = Some(delim);
            }
            Token::RedirErr => {
                let f = next_word(&mut iter, "2>")?;
                cur.stderr_file = Some(f);
                cur.append_err = false;
            }
            Token::RedirErrAppend => {
                let f = next_word(&mut iter, "2>>")?;
                cur.stderr_file = Some(f);
                cur.append_err = true;
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
                background = true;
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

#[cfg(test)]
mod tests {
    use super::super::tokenizer::tokenize;
    use super::*;

    fn parse(line: &str) -> CommandList {
        let tokens = tokenize(line);
        build_command_list(&tokens).unwrap()
    }

    #[test]
    fn single_command() {
        let cl = parse("echo hello");
        assert_eq!(cl.segments.len(), 1);
        assert_eq!(cl.segments[0].connector, Connector::Start);
        assert_eq!(cl.segments[0].pipeline.cmds.len(), 1);
        assert_eq!(cl.segments[0].pipeline.cmds[0].argv, vec!["echo", "hello"]);
    }

    #[test]
    fn pipeline_two() {
        let cl = parse("cat | grep foo");
        assert_eq!(cl.segments.len(), 1);
        assert_eq!(cl.segments[0].pipeline.cmds.len(), 2);
        assert_eq!(cl.segments[0].pipeline.cmds[0].argv, vec!["cat"]);
        assert_eq!(cl.segments[0].pipeline.cmds[1].argv, vec!["grep", "foo"]);
    }

    #[test]
    fn pipeline_three() {
        let cl = parse("echo a | cat | cat");
        assert_eq!(cl.segments[0].pipeline.cmds.len(), 3);
    }

    #[test]
    fn semicolon_split() {
        let cl = parse("echo a ; echo b");
        assert_eq!(cl.segments.len(), 2);
        assert_eq!(cl.segments[0].connector, Connector::Start);
        assert_eq!(cl.segments[1].connector, Connector::Sequential);
    }

    #[test]
    fn and_if() {
        let cl = parse("true && echo yes");
        assert_eq!(cl.segments.len(), 2);
        assert_eq!(cl.segments[1].connector, Connector::AndIf);
    }

    #[test]
    fn or_if() {
        let cl = parse("false || echo no");
        assert_eq!(cl.segments.len(), 2);
        assert_eq!(cl.segments[1].connector, Connector::OrIf);
    }

    #[test]
    fn background() {
        let cl = parse("sleep 10 &");
        assert_eq!(cl.segments.len(), 1);
        assert!(cl.segments[0].pipeline.background);
    }

    #[test]
    fn redirect_out() {
        let cl = parse("echo > f.txt");
        let cmd = &cl.segments[0].pipeline.cmds[0];
        assert_eq!(cmd.stdout_file.as_deref(), Some("f.txt"));
        assert!(!cmd.append);
    }

    #[test]
    fn redirect_append() {
        let cl = parse("echo >> f.txt");
        let cmd = &cl.segments[0].pipeline.cmds[0];
        assert_eq!(cmd.stdout_file.as_deref(), Some("f.txt"));
        assert!(cmd.append);
    }

    #[test]
    fn redirect_in() {
        let cl = parse("cat < in.txt");
        let cmd = &cl.segments[0].pipeline.cmds[0];
        assert_eq!(cmd.stdin_file.as_deref(), Some("in.txt"));
    }

    #[test]
    fn syntax_error_empty_pipe() {
        let tokens = vec![Token::Pipe];
        assert!(build_command_list(&tokens).is_err());
    }

    #[test]
    fn syntax_error_missing_redirect_target() {
        let tokens = vec![Token::Word("echo".into()), Token::RedirOut];
        assert!(build_command_list(&tokens).is_err());
    }

    #[test]
    fn complex_chain() {
        let cl = parse("false || echo a && echo b");
        assert_eq!(cl.segments.len(), 3);
        assert_eq!(cl.segments[0].connector, Connector::Start);
        assert_eq!(cl.segments[1].connector, Connector::OrIf);
        assert_eq!(cl.segments[2].connector, Connector::AndIf);
    }
}
