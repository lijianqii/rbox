//! Tab 补全：命令补全 + 文件补全。

use crate::applet;
use std::io::{self, Write};

/// 补全结果：`(补全后的行, 是否打印了多匹配列表)`。
pub fn tab_complete(line: &str) -> (String, bool) {
    let word_start = find_last_word_start(line);
    let prefix = &line[word_start..];
    if prefix.is_empty() {
        return (line.to_string(), false);
    }

    // 判断是命令补全还是文件补全：
    // - 第一个词且不含 / -> 命令补全
    // - 管道 | ; && || 后的新命令 -> 命令补全
    // - 含 / 的第一个词（如 /bin/ls）-> 文件补全
    // - 其他 -> 文件补全
    let is_first_word = line[..word_start].trim().is_empty();
    let is_path = prefix.contains('/');
    let after_operator = {
        let before = line[..word_start].trim_end();
        before.ends_with('|')
            || before.ends_with(';')
            || before.ends_with("&&")
            || before.ends_with("||")
    };

    let matches = if (is_first_word || after_operator) && !is_path {
        complete_command(prefix)
    } else {
        complete_file(prefix)
    };

    if matches.is_empty() {
        return (line.to_string(), false);
    }

    if matches.len() == 1 {
        // 唯一匹配：补全整个词
        let completion = &matches[0];
        let mut new_line = line[..word_start].to_string();
        new_line.push_str(completion);
        // 目录补全后不加空格（用户可能要继续输入子路径）
        if !completion.ends_with('/') {
            new_line.push(' ');
        }
        (new_line, false)
    } else {
        // 多匹配：先补全到公共前缀
        let common = common_prefix(&matches);
        if common.len() > prefix.len() {
            let mut new_line = line[..word_start].to_string();
            new_line.push_str(&common);
            (new_line, false)
        } else {
            // 无法继续补全，显示所有匹配（列格式排列）
            let displays: Vec<String> = matches
                .iter()
                .map(|m| {
                    let name = std::path::Path::new(m.trim_end_matches('/'))
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| m.clone());
                    if m.ends_with('/') {
                        format!("{}/", name)
                    } else {
                        name
                    }
                })
                .collect();
            print_completions(&displays);
            (line.to_string(), true)
        }
    }
}

/// 命令补全：匹配内置 applet 名 + 内置命令 + PATH 下的可执行文件。
fn complete_command(prefix: &str) -> Vec<String> {
    let mut matches = Vec::new();

    // 内置 applet
    for applet in applet::APPLETS {
        let name = applet.name();
        if name.starts_with(prefix) {
            matches.push(name.to_string());
        }
    }

    // 内置命令（shell builtin）
    for builtin in &["cd", "exit", "export", "unset", "pwd", "history"] {
        if builtin.starts_with(prefix) && !matches.iter().any(|m| m == *builtin) {
            matches.push(builtin.to_string());
        }
    }

    // PATH 下的可执行文件
    if let Ok(paths) = std::env::var("PATH") {
        for dir in paths.split(':') {
            if dir.is_empty() {
                continue;
            }
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with(prefix) && !matches.iter().any(|m| m == &name) {
                        matches.push(name);
                    }
                }
            }
        }
    }

    matches.sort();
    matches.dedup();
    matches
}

/// 文件补全：匹配文件系统路径。
fn complete_file(prefix: &str) -> Vec<String> {
    let path = std::path::Path::new(prefix);

    let (search_dir, prefix_name, base) = if prefix.ends_with('/') {
        // /proc/ -> 在 /proc/ 中搜索，空前缀
        (path.to_path_buf(), String::new(), prefix.to_string())
    } else if let Some(parent) = path.parent() {
        if parent.as_os_str().is_empty() {
            // pro -> 在 . 中搜索，前缀 pro
            (
                std::path::PathBuf::from("."),
                prefix.to_string(),
                String::new(),
            )
        } else {
            // /pro -> 在 / 中搜索，前缀 pro，base /
            // /bin/l -> 在 /bin 中搜索，前缀 l，base /bin/
            let fname = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let b = if parent == std::path::Path::new("/") {
                "/".to_string()
            } else {
                format!("{}/", parent.to_string_lossy())
            };
            (parent.to_path_buf(), fname, b)
        }
    } else {
        (
            std::path::PathBuf::from("."),
            prefix.to_string(),
            String::new(),
        )
    };

    let entries = match std::fs::read_dir(&search_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut matches: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix_name) {
            let full = format!("{}{}", base, name);
            // 目录加尾随 /
            if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                matches.push(format!("{}/", full));
            } else {
                matches.push(full);
            }
        }
    }
    matches.sort();
    matches
}

/// 计算字符串列表的公共前缀。
fn common_prefix(strs: &[String]) -> String {
    if strs.is_empty() {
        return String::new();
    }
    let first = &strs[0];
    let mut len = first.len();
    for s in &strs[1..] {
        len = len.min(s.len());
        let mut i = 0;
        while i < len && first.as_bytes()[i] == s.as_bytes()[i] {
            i += 1;
        }
        len = i;
        if len == 0 {
            break;
        }
    }
    first[..len].to_string()
}

/// 以列格式打印补全选项（类似 bash compgen）。
fn print_completions(items: &[String]) {
    if items.is_empty() {
        return;
    }
    println!();

    let max_len = items.iter().map(|s| s.chars().count()).max().unwrap_or(0);
    let col_width = max_len + 2;
    let term_width = 80usize;
    let cols = (term_width / col_width).max(1);

    for (i, item) in items.iter().enumerate() {
        let pad = col_width - item.chars().count();
        print!("{}{}", item, " ".repeat(pad));
        if (i + 1) % cols == 0 || i == items.len() - 1 {
            println!();
        }
    }
    let _ = io::stdout().flush();
}

/// 找到最后一个词的开始位置（用于 Tab 补全时提取当前正在输入的词）。
fn find_last_word_start(line: &str) -> usize {
    let bytes = line.as_bytes();
    let mut i = bytes.len();
    // 跳过尾部空白
    while i > 0 && (bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') {
        i -= 1;
    }
    // 找到词的开始
    while i > 0 {
        let c = bytes[i - 1];
        if c == b' ' || c == b'\t' || c == b'|' || c == b';' || c == b'&' || c == b'>' || c == b'<'
        {
            break;
        }
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── find_last_word_start ────────────────────

    #[test]
    fn word_start_simple() {
        assert_eq!(find_last_word_start("echo hel"), 5);
    }

    #[test]
    fn word_start_first_word() {
        assert_eq!(find_last_word_start("ech"), 0);
    }

    #[test]
    fn word_start_after_pipe() {
        assert_eq!(find_last_word_start("cat | gr"), 6);
    }

    #[test]
    fn word_start_after_semicolon() {
        assert_eq!(find_last_word_start("echo a; ec"), 8);
    }

    #[test]
    fn word_start_after_redirect() {
        assert_eq!(find_last_word_start("echo > f"), 7);
    }

    #[test]
    fn word_start_trailing_space() {
        // trailing space -> word_start at end (empty prefix)
        assert_eq!(find_last_word_start("echo "), 0);
    }

    // ─── common_prefix ──────────────────────────

    #[test]
    fn common_prefix_basic() {
        assert_eq!(common_prefix(&["abc".into(), "abd".into()]), "ab");
    }

    #[test]
    fn common_prefix_identical() {
        assert_eq!(common_prefix(&["abc".into(), "abc".into()]), "abc");
    }

    #[test]
    fn common_prefix_no_common() {
        assert_eq!(common_prefix(&["abc".into(), "xyz".into()]), "");
    }

    #[test]
    fn common_prefix_single() {
        assert_eq!(common_prefix(&["abc".into()]), "abc");
    }

    #[test]
    fn common_prefix_empty() {
        assert_eq!(common_prefix(&[]), "");
    }

    // ─── tab_complete ───────────────────────────

    #[test]
    fn complete_empty_line_noop() {
        let (line, printed) = tab_complete("");
        assert_eq!(line, "");
        assert!(!printed);
    }

    #[test]
    fn complete_command_echo() {
        let (line, printed) = tab_complete("ec");
        // Should complete to "echo " (with trailing space for file)
        assert_eq!(line, "echo ");
        assert!(!printed);
    }

    #[test]
    fn complete_command_partial_no_match() {
        let (line, _) = tab_complete("zzzz");
        assert_eq!(line, "zzzz");
    }

    #[test]
    fn complete_after_pipe() {
        let (line, printed) = tab_complete("cat | ec");
        assert_eq!(line, "cat | echo ");
        assert!(!printed);
    }

    #[test]
    fn complete_after_semicolon() {
        let (line, _) = tab_complete("echo a; ec");
        assert_eq!(line, "echo a; echo ");
    }

    // ─── complete_file 路径补全 ─────────────────

    #[test]
    fn file_complete_root_prefix() {
        // /pro -> 在 / 中搜索 pro
        let matches = complete_file("/pro");
        // 至少应该匹配到 /proc/
        assert!(matches.iter().any(|m| m == "/proc/"));
    }

    #[test]
    fn file_complete_nested_path() {
        // /etc/pa -> 在 /etc 中搜索 pa
        let matches = complete_file("/etc/pa");
        // 应该至少匹配到 /etc/passwd
        assert!(matches.iter().any(|m| m == "/etc/passwd"));
    }

    #[test]
    fn file_complete_trailing_slash() {
        // /bin/ -> 在 /bin 中搜索空前缀，列出所有文件
        let matches = complete_file("/bin/");
        assert!(!matches.is_empty());
    }

    #[test]
    fn file_complete_no_match() {
        let matches = complete_file("/zzzznonexistent");
        assert!(matches.is_empty());
    }
}
