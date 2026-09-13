//! glob 匹配共享工具：shell 通配符展开与 `find -name` 共用。

/// glob 保护标记：分词时引号内/反斜杠转义的 glob 元字符（`*` `?` `[`）
/// 前插入此标记，匹配时按字面处理，展开后由 shell 的 unescape 移除。
pub(crate) const GLOB_ESCAPE: char = '\u{1}';

/// glob 匹配：支持 `*` `?` `[abc]` `[a-z]` `[!abc]`，以及 `GLOB_ESCAPE`
/// 保护的字面元字符。
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob_match_inner(&p, &t)
}

fn glob_match_inner(p: &[char], t: &[char]) -> bool {
    if p.is_empty() {
        return t.is_empty();
    }
    match p[0] {
        GLOB_ESCAPE => {
            // 引号/转义保护的字面字符：p[1] 与 t[0] 精确匹配
            if p.len() < 2 || t.is_empty() || p[1] != t[0] {
                return false;
            }
            glob_match_inner(&p[2..], &t[1..])
        }
        '*' => {
            if p.len() == 1 {
                return true;
            }
            for i in 0..=t.len() {
                if glob_match_inner(&p[1..], &t[i..]) {
                    return true;
                }
            }
            false
        }
        '?' => {
            if t.is_empty() {
                return false;
            }
            glob_match_inner(&p[1..], &t[1..])
        }
        '[' => {
            if t.is_empty() {
                return false;
            }
            let mut idx = 1;
            let mut negate = false;
            if idx < p.len() && p[idx] == '!' {
                negate = true;
                idx += 1;
            }
            let mut matched = false;
            while idx < p.len() && p[idx] != ']' {
                if idx + 2 < p.len() && p[idx + 1] == '-' && p[idx + 2] != ']' {
                    if t[0] >= p[idx] && t[0] <= p[idx + 2] {
                        matched = true;
                    }
                    idx += 3;
                } else {
                    if t[0] == p[idx] {
                        matched = true;
                    }
                    idx += 1;
                }
            }
            let rest = if idx < p.len() {
                &p[idx + 1..]
            } else {
                &p[idx..]
            };
            if matched != negate {
                glob_match_inner(rest, &t[1..])
            } else {
                false
            }
        }
        _ => {
            if t.is_empty() || p[0] != t[0] {
                return false;
            }
            glob_match_inner(&p[1..], &t[1..])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_matches_any() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*.txt", "file.txt"));
        assert!(!glob_match("*.txt", "file.rs"));
        assert!(glob_match("a*b*c", "aXXbYYc"));
    }

    #[test]
    fn question_matches_single() {
        assert!(glob_match("?", "a"));
        assert!(!glob_match("?", "ab"));
        assert!(glob_match("a?c", "abc"));
    }

    #[test]
    fn bracket_set_and_range() {
        assert!(glob_match("[abc]", "a"));
        assert!(!glob_match("[abc]", "d"));
        assert!(glob_match("[0-9]", "5"));
        assert!(!glob_match("[0-9]", "a"));
        assert!(glob_match("[!abc]", "d"));
        assert!(!glob_match("[!abc]", "a"));
    }

    #[test]
    fn escape_marker_is_literal() {
        assert!(glob_match(&format!("{}*", GLOB_ESCAPE), "*"));
        assert!(!glob_match(&format!("{}*", GLOB_ESCAPE), "abc"));
        assert!(glob_match(&format!("a{}*b*", GLOB_ESCAPE), "a*bcd"));
    }

    #[test]
    fn utf8_names_match_charwise() {
        assert!(glob_match("文*", "文件名"));
        assert!(glob_match("*名", "文件名"));
        assert!(glob_match("*件*", "文件名"));
        assert!(!glob_match("*件", "文件名"));
        assert!(glob_match("?", "文")); // ? 匹配任意单个字符（含多字节）
        assert!(!glob_match("?", "文字"));
    }
}
