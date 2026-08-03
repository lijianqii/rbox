//! 分词器：将输入行切分为 Token 序列。

use super::types::Token;

/// 将输入行切分为 Token 序列。
///
/// 支持双引号、单引号、反斜杠转义、续行、注释。
pub fn tokenize(line: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_dquote = false;
    let mut in_squote = false;
    let mut in_token = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if in_squote {
            match c {
                '\'' => {
                    in_squote = false;
                    in_token = true;
                }
                _ => {
                    cur.push(c);
                    in_token = true;
                }
            }
            continue;
        }

        if in_dquote {
            match c {
                '"' => {
                    in_dquote = false;
                    in_token = true;
                }
                '\\' => {
                    if let Some(&next) = chars.peek() {
                        match next {
                            '$' | '`' | '"' | '\\' => {
                                chars.next();
                                cur.push(next);
                            }
                            '\n' => {
                                chars.next();
                            }
                            _ => cur.push('\\'),
                        }
                    } else {
                        cur.push('\\');
                    }
                }
                _ => cur.push(c),
            }
            continue;
        }

        match c {
            '#' if !in_token => break,
            '\\' => {
                if let Some(next) = chars.next() {
                    if next != '\n' {
                        cur.push(next);
                        in_token = true;
                    }
                }
            }
            '\'' => {
                in_squote = true;
                in_token = true;
            }
            '"' => {
                in_dquote = true;
                in_token = true;
            }
            '>' => {
                flush_word(&mut tokens, &mut cur, &mut in_token);
                if chars.peek() == Some(&'>') {
                    chars.next();
                    tokens.push(Token::RedirAppend);
                } else {
                    tokens.push(Token::RedirOut);
                }
            }
            '<' => {
                flush_word(&mut tokens, &mut cur, &mut in_token);
                tokens.push(Token::RedirIn);
            }
            '|' => {
                flush_word(&mut tokens, &mut cur, &mut in_token);
                if chars.peek() == Some(&'|') {
                    chars.next();
                    tokens.push(Token::OrIf);
                } else {
                    tokens.push(Token::Pipe);
                }
            }
            '&' => {
                flush_word(&mut tokens, &mut cur, &mut in_token);
                if chars.peek() == Some(&'&') {
                    chars.next();
                    tokens.push(Token::AndIf);
                } else {
                    tokens.push(Token::Background);
                }
            }
            ';' => {
                flush_word(&mut tokens, &mut cur, &mut in_token);
                tokens.push(Token::Semicolon);
            }
            ' ' | '\t' => {
                flush_word(&mut tokens, &mut cur, &mut in_token);
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

/// 将当前累积的 word 推入 tokens（如果有）。
fn flush_word(tokens: &mut Vec<Token>, cur: &mut String, in_token: &mut bool) {
    if *in_token {
        tokens.push(Token::Word(std::mem::take(cur)));
        *in_token = false;
    }
}
