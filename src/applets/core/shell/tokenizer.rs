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
                if let Some(next) = chars.next()
                    && next != '\n'
                {
                    cur.push(next);
                    in_token = true;
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
            '2' if chars.peek() == Some(&'>') => {
                // stderr 重定向：2> 或 2>>
                chars.next(); // consume '>'
                flush_word(&mut tokens, &mut cur, &mut in_token);
                if chars.peek() == Some(&'>') {
                    chars.next();
                    tokens.push(Token::RedirErrAppend);
                } else {
                    tokens.push(Token::RedirErr);
                }
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
                if chars.peek() == Some(&'<') {
                    chars.next();
                    tokens.push(Token::RedirHereDoc);
                } else {
                    tokens.push(Token::RedirIn);
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_words() {
        let tokens = tokenize("echo hello world");
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into()),
                Token::Word("hello".into()),
                Token::Word("world".into()),
            ]
        );
    }

    #[test]
    fn double_quotes() {
        let tokens = tokenize("echo \"hello world\" ");
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into()),
                Token::Word("hello world".into()),
            ]
        );
    }

    #[test]
    fn single_quotes() {
        let tokens = tokenize("echo 'a $B c'");
        assert_eq!(
            tokens,
            vec![Token::Word("echo".into()), Token::Word("a $B c".into()),]
        );
    }

    #[test]
    fn backslash_escape() {
        let tokens = tokenize(r"echo a\ b");
        assert_eq!(
            tokens,
            vec![Token::Word("echo".into()), Token::Word("a b".into()),]
        );
    }

    #[test]
    fn pipe_operator() {
        let tokens = tokenize("cat | grep foo");
        assert_eq!(
            tokens,
            vec![
                Token::Word("cat".into()),
                Token::Pipe,
                Token::Word("grep".into()),
                Token::Word("foo".into()),
            ]
        );
    }

    #[test]
    fn redirect_operators() {
        let tokens = tokenize("echo > f && cat >> f");
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into()),
                Token::RedirOut,
                Token::Word("f".into()),
                Token::AndIf,
                Token::Word("cat".into()),
                Token::RedirAppend,
                Token::Word("f".into()),
            ]
        );
    }

    #[test]
    fn input_redirect() {
        let tokens = tokenize("cat < input.txt");
        assert_eq!(
            tokens,
            vec![
                Token::Word("cat".into()),
                Token::RedirIn,
                Token::Word("input.txt".into()),
            ]
        );
    }

    #[test]
    fn semicolons_and_background() {
        let tokens = tokenize("a ; b & c");
        assert_eq!(
            tokens,
            vec![
                Token::Word("a".into()),
                Token::Semicolon,
                Token::Word("b".into()),
                Token::Background,
                Token::Word("c".into()),
            ]
        );
    }

    #[test]
    fn or_if() {
        let tokens = tokenize("false || echo fail");
        assert_eq!(
            tokens,
            vec![
                Token::Word("false".into()),
                Token::OrIf,
                Token::Word("echo".into()),
                Token::Word("fail".into()),
            ]
        );
    }

    #[test]
    fn comment_ignored() {
        let tokens = tokenize("echo hi # this is a comment");
        assert_eq!(
            tokens,
            vec![Token::Word("echo".into()), Token::Word("hi".into()),]
        );
    }

    #[test]
    fn empty_input() {
        let tokens = tokenize("");
        assert!(tokens.is_empty());
    }

    #[test]
    fn whitespace_only() {
        let tokens = tokenize("   \t  ");
        assert!(tokens.is_empty());
    }

    #[test]
    fn multiple_spaces() {
        let tokens = tokenize("echo    a     b");
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into()),
                Token::Word("a".into()),
                Token::Word("b".into()),
            ]
        );
    }

    // ─── stderr 重定向 ─────────────────────────

    #[test]
    fn stderr_redirect() {
        let tokens = tokenize("echo hi 2> /tmp/err");
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into()),
                Token::Word("hi".into()),
                Token::RedirErr,
                Token::Word("/tmp/err".into()),
            ]
        );
    }

    #[test]
    fn stderr_redirect_append() {
        let tokens = tokenize("echo hi 2>> /tmp/err");
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into()),
                Token::Word("hi".into()),
                Token::RedirErrAppend,
                Token::Word("/tmp/err".into()),
            ]
        );
    }

    // ─── here-doc ──────────────────────────────

    #[test]
    fn heredoc_token() {
        let tokens = tokenize("cat <<EOF");
        assert_eq!(
            tokens,
            vec![
                Token::Word("cat".into()),
                Token::RedirHereDoc,
                Token::Word("EOF".into()),
            ]
        );
    }

    #[test]
    fn combined_redirects() {
        let tokens = tokenize("cmd < in > out 2> err");
        assert_eq!(
            tokens,
            vec![
                Token::Word("cmd".into()),
                Token::RedirIn,
                Token::Word("in".into()),
                Token::RedirOut,
                Token::Word("out".into()),
                Token::RedirErr,
                Token::Word("err".into()),
            ]
        );
    }

    // ─── 数字 2 作为普通参数 ───────────────────

    #[test]
    fn digit_two_as_word() {
        // "2" not followed by ">" should be a word
        let tokens = tokenize("echo 2");
        assert_eq!(
            tokens,
            vec![Token::Word("echo".into()), Token::Word("2".into()),]
        );
    }

    // ─── 引号嵌套 ──────────────────────────────

    #[test]
    fn double_quote_with_single_inside() {
        let tokens = tokenize("echo \"it's\"");
        assert_eq!(
            tokens,
            vec![Token::Word("echo".into()), Token::Word("it's".into()),]
        );
    }

    #[test]
    fn single_quote_with_double_inside() {
        let tokens = tokenize("echo 'say \"hi\"'");
        assert_eq!(
            tokens,
            vec![Token::Word("echo".into()), Token::Word("say \"hi\"".into()),]
        );
    }

    #[test]
    fn mixed_quotes_concatenation() {
        // 'abc'"def" -> abcdef (no space between)
        let tokens = tokenize("echo 'abc'\"def\"");
        assert_eq!(
            tokens,
            vec![Token::Word("echo".into()), Token::Word("abcdef".into()),]
        );
    }

    // ─── 转义字符 ──────────────────────────────

    #[test]
    fn escaped_pipe_as_word() {
        let tokens = tokenize(r"echo a\|b");
        // \| inside word -> literal |
        assert_eq!(
            tokens,
            vec![Token::Word("echo".into()), Token::Word("a|b".into()),]
        );
    }

    #[test]
    fn escaped_semicolon_as_word() {
        let tokens = tokenize(r"echo a\;b");
        assert_eq!(
            tokens,
            vec![Token::Word("echo".into()), Token::Word("a;b".into()),]
        );
    }

    // ─── 注释 ──────────────────────────────────

    #[test]
    fn comment_at_end() {
        let tokens = tokenize("echo hi # this is a comment");
        assert_eq!(
            tokens,
            vec![Token::Word("echo".into()), Token::Word("hi".into()),]
        );
    }

    #[test]
    fn full_line_comment() {
        let tokens = tokenize("# just a comment");
        assert!(tokens.is_empty());
    }

    #[test]
    fn comment_after_operator() {
        let tokens = tokenize("echo a; # comment");
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into()),
                Token::Word("a".into()),
                Token::Semicolon,
            ]
        );
    }

    // ─── 续行 ──────────────────────────────────

    #[test]
    fn line_continuation() {
        let tokens = tokenize("echo a\\\nb");
        assert_eq!(
            tokens,
            vec![Token::Word("echo".into()), Token::Word("ab".into()),]
        );
    }

    // ─── 空输入 ────────────────────────────────

    #[test]
    fn only_whitespace() {
        let tokens = tokenize("   \t  ");
        assert!(tokens.is_empty());
    }

    #[test]
    fn only_comment() {
        let tokens = tokenize("  # comment only  ");
        assert!(tokens.is_empty());
    }
}
