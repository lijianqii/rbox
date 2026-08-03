//! 文本处理共享工具（head/tail 共用）。
use std::io::{Read, Write};

/// 解析 `[-n N]` 与文件列表；`-` 视为 stdin（忽略）。
pub(crate) fn parse_n_files<'a>(args: &'a [String], app: &str) -> (usize, Vec<&'a String>) {
    let mut n: usize = 10;
    let mut files: Vec<&String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" => {
                i += 1;
                if i < args.len() {
                    n = args[i].parse().unwrap_or(10);
                }
            }
            s if s.starts_with("-n") && s.len() > 2 => {
                n = s[2..].parse().unwrap_or(10);
            }
            "-" => {}
            s if s.starts_with('-') && s.len() > 1 => {
                eprintln!("{}: unknown option: {}", app, s);
            }
            _ => files.push(&args[i]),
        }
        i += 1;
    }
    (n, files)
}

/// 遍历输入：无文件读 stdin，多文件打印 `==> name <==` 头，逐个调用 f。
pub(crate) fn each_input(
    files: &[&String],
    app: &str,
    out: &mut std::io::StdoutLock,
    mut f: impl FnMut(&str, &mut std::io::StdoutLock),
) {
    if files.is_empty() {
        let mut buf = String::new();
        if std::io::stdin().lock().read_to_string(&mut buf).is_ok() {
            f(&buf, out);
        }
    } else {
        for file in files {
            if files.len() > 1 {
                let _ = writeln!(out, "==> {} <==", file);
            }
            match std::fs::read_to_string(file) {
                Ok(content) => f(&content, out),
                Err(e) => eprintln!("{}: {}: {}", app, file, e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_n_default() {
        let args: Vec<String> = vec![];
        let (n, files) = parse_n_files(&args, "head");
        assert_eq!(n, 10);
        assert!(files.is_empty());
    }

    #[test]
    fn parse_n_explicit() {
        let args = vec!["-n".to_string(), "5".to_string()];
        let (n, _) = parse_n_files(&args, "head");
        assert_eq!(n, 5);
    }

    #[test]
    fn parse_n_combined() {
        let args = vec!["-n3".to_string()];
        let (n, _) = parse_n_files(&args, "head");
        assert_eq!(n, 3);
    }

    #[test]
    fn parse_files() {
        let args = vec![
            "-n".to_string(),
            "2".to_string(),
            "a.txt".to_string(),
            "b.txt".to_string(),
        ];
        let (n, files) = parse_n_files(&args, "head");
        assert_eq!(n, 2);
        assert_eq!(files.len(), 2);
    }
}
