//! 别名（alias/unalias）：命令名替换与查询。
//!
//! 别名表为进程内全局状态（shell 单线程执行，Mutex 仅用于满足 Sync）。
//! 展开发生在分词前、按"命令位置的首词"进行（`;` `|` `&&` `||` `&` 之后），
//! 单引号/双引号内不展开，最多链式展开 16 次防循环。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

fn aliases() -> &'static Mutex<HashMap<String, String>> {
    static ALIASES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    ALIASES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 定义别名。名字含 `=` 或为空时返回 Err。
pub(crate) fn set_alias(name: &str, value: &str) -> Result<(), String> {
    if name.is_empty() || name.contains('=') {
        return Err(format!("invalid alias name: '{}'", name));
    }
    aliases()
        .lock()
        .map_err(|_| "alias table poisoned".to_string())?
        .insert(name.to_string(), value.to_string());
    Ok(())
}

/// 删除别名；`-a` 删除全部。
pub(crate) fn unalias(name: &str) {
    if let Ok(mut map) = aliases().lock() {
        if name == "-a" {
            map.clear();
        } else {
            map.remove(name);
        }
    }
}

/// 查询别名。
pub(crate) fn get_alias(name: &str) -> Option<String> {
    aliases().lock().ok()?.get(name).cloned()
}

/// 列出全部别名（按名字排序的 `name=value`）。
pub(crate) fn list_aliases() -> Vec<String> {
    let Ok(map) = aliases().lock() else {
        return Vec::new();
    };
    let mut items: Vec<(String, String)> =
        map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    items.sort();
    items
        .into_iter()
        .map(|(k, v)| format!("alias {}='{}'", k, v))
        .collect()
}

/// 文本级别名展开：替换每个"命令位置"的首个词，直到无可替换。
pub(crate) fn expand_alias(line: &str) -> String {
    let Ok(map) = aliases().lock() else {
        return line.to_string();
    };
    if map.is_empty() {
        return line.to_string();
    }
    let mut current = line.to_string();
    // 已展开过的别名名（防止自引用/互引用无限循环；每个名字只展开一次）
    let mut used: Vec<String> = Vec::new();
    for _ in 0..16 {
        match replace_first_alias(&current, &map) {
            Some((next, name)) => {
                if used.contains(&name) {
                    break;
                }
                used.push(name);
                current = next;
            }
            None => break,
        }
    }
    current
}

/// 找到第一个处于命令位置且命中别名的词并替换；
/// 返回 (替换后的行, 命中的别名名)；无则返回 None。
fn replace_first_alias(line: &str, map: &HashMap<String, String>) -> Option<(String, String)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_squote = false;
    let mut in_dquote = false;
    let mut at_command_start = true;
    while i < bytes.len() {
        let c = bytes[i];
        if in_squote {
            if c == b'\'' {
                in_squote = false;
            }
            i += 1;
            continue;
        }
        if in_dquote {
            match c {
                b'"' => in_dquote = false,
                b'\\' => i += 1,
                _ => {}
            }
            i += 1;
            continue;
        }
        match c {
            b'\'' => {
                in_squote = true;
                at_command_start = false;
                i += 1;
            }
            b'"' => {
                in_dquote = true;
                at_command_start = false;
                i += 1;
            }
            b'\\' => {
                i += 2;
                at_command_start = false;
            }
            b' ' | b'\t' => i += 1,
            b';' | b'|' | b'&' | b'(' => {
                at_command_start = true;
                i += 1;
            }
            _ => {
                if !at_command_start {
                    i += 1;
                    continue;
                }
                let start = i;
                while i < bytes.len()
                    && !matches!(
                        bytes[i],
                        b' ' | b'\t' | b';' | b'|' | b'&' | b'<' | b'>' | b'(' | b')'
                    )
                {
                    i += 1;
                }
                let word = &line[start..i];
                if let Some(rep) = map.get(word) {
                    let mut out = String::with_capacity(line.len() + rep.len());
                    out.push_str(&line[..start]);
                    out.push_str(rep);
                    out.push_str(&line[i..]);
                    return Some((out, word.to_string()));
                }
                at_command_start = false;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个测试使用独立别名名，避免全局表互相干扰。
    #[test]
    fn set_get_list_unalias() {
        set_alias("rbox_test_ll", "ls -l").unwrap();
        assert_eq!(get_alias("rbox_test_ll").as_deref(), Some("ls -l"));
        assert!(list_aliases().iter().any(|l| l.contains("rbox_test_ll")));
        unalias("rbox_test_ll");
        assert_eq!(get_alias("rbox_test_ll"), None);
    }

    #[test]
    fn invalid_alias_name_rejected() {
        assert!(set_alias("", "x").is_err());
        assert!(set_alias("a=b", "x").is_err());
    }

    #[test]
    fn expand_first_word_only() {
        set_alias("rbox_test_e", "echo").unwrap();
        assert_eq!(expand_alias("rbox_test_e hi"), "echo hi");
        // 参数位置不展开
        assert_eq!(expand_alias("echo rbox_test_e"), "echo rbox_test_e");
        unalias("rbox_test_e");
    }

    #[test]
    fn expand_after_operators() {
        set_alias("rbox_test_o", "echo ok").unwrap();
        assert_eq!(expand_alias("true; rbox_test_o"), "true; echo ok");
        assert_eq!(expand_alias("false || rbox_test_o"), "false || echo ok");
        assert_eq!(expand_alias("true && rbox_test_o"), "true && echo ok");
        assert_eq!(expand_alias("cat x | rbox_test_o"), "cat x | echo ok");
        unalias("rbox_test_o");
    }

    #[test]
    fn expand_not_inside_quotes() {
        set_alias("rbox_test_q", "echo").unwrap();
        assert_eq!(expand_alias("echo 'rbox_test_q'"), "echo 'rbox_test_q'");
        assert_eq!(expand_alias("echo \"rbox_test_q\""), "echo \"rbox_test_q\"");
        unalias("rbox_test_q");
    }

    #[test]
    fn expand_self_reference_not_looped() {
        set_alias("rbox_test_self", "rbox_test_self -l").unwrap();
        assert_eq!(expand_alias("rbox_test_self"), "rbox_test_self -l");
        unalias("rbox_test_self");
    }

    #[test]
    fn expand_chain() {
        set_alias("rbox_test_a1", "rbox_test_a2").unwrap();
        set_alias("rbox_test_a2", "echo chained").unwrap();
        assert_eq!(expand_alias("rbox_test_a1"), "echo chained");
        unalias("rbox_test_a1");
        unalias("rbox_test_a2");
    }
}
