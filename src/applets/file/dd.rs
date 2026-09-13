//! `dd` - 按块复制数据。
//!
//! 用法：dd [if=FILE] [of=FILE] [bs=N] [count=N] [skip=N] [seek=N] [status=none]
//! 大小后缀：k/K=1024，M=1024²，G=1024³（无后缀为字节）。

use crate::applet::Applet;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::process::ExitCode;

pub struct Dd;
pub static DD: &Dd = &Dd;

/// 解析大小（支持 k/K/M/G 后缀）。
pub(crate) fn parse_size(s: &str) -> Option<u64> {
    let (num, mult) = match s.chars().last()? {
        'k' | 'K' => (&s[..s.len() - 1], 1024u64),
        'M' | 'm' => (&s[..s.len() - 1], 1024 * 1024),
        'G' | 'g' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    num.parse::<u64>().ok().map(|n| n * mult)
}

/// dd 选项。
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct DdOpts {
    pub(crate) ifile: Option<String>,
    pub(crate) ofile: Option<String>,
    pub(crate) bs: u64,
    pub(crate) count: Option<u64>,
    pub(crate) skip: u64,
    pub(crate) seek: u64,
    pub(crate) quiet: bool,
}

/// 解析参数。
pub(crate) fn parse_args(args: &[String]) -> Result<DdOpts, String> {
    let mut o = DdOpts {
        bs: 512,
        ..Default::default()
    };
    for a in args {
        let (k, v) = a
            .split_once('=')
            .ok_or_else(|| format!("invalid argument: {}", a))?;
        match k {
            "if" => o.ifile = Some(v.to_string()),
            "of" => o.ofile = Some(v.to_string()),
            "bs" => o.bs = parse_size(v).ok_or_else(|| format!("invalid bs: {}", v))?,
            "count" => o.count = Some(v.parse().map_err(|_| format!("invalid count: {}", v))?),
            "skip" => o.skip = v.parse().map_err(|_| format!("invalid skip: {}", v))?,
            "seek" => o.seek = v.parse().map_err(|_| format!("invalid seek: {}", v))?,
            "status" => {
                if v == "none" {
                    o.quiet = true;
                }
            }
            other => return Err(format!("unknown operand: {}", other)),
        }
    }
    if o.bs == 0 {
        return Err("bs must be > 0".to_string());
    }
    Ok(o)
}

/// 执行复制，返回 (读取字节, 写入字节, 完整块数, 部分块数)。
pub(crate) fn run_dd(o: &DdOpts) -> std::io::Result<(u64, u64, u64, u64)> {
    let mut input: Box<dyn Read> = match &o.ifile {
        Some(f) => Box::new(std::fs::File::open(f)?),
        None => Box::new(std::io::stdin().lock()),
    };
    if o.skip > 0 {
        let mut skipped = 0u64;
        let mut buf = vec![0u8; o.bs as usize];
        while skipped < o.skip {
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            skipped += 1;
        }
    }
    let mut output: Box<dyn Write> = match &o.ofile {
        Some(f) => {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(f)?;
            if o.seek > 0 {
                file.seek(SeekFrom::Start(o.seek * o.bs))?;
            }
            Box::new(file)
        }
        None => Box::new(std::io::stdout().lock()),
    };

    let mut buf = vec![0u8; o.bs as usize];
    let mut read_total = 0u64;
    let mut write_total = 0u64;
    let mut full = 0u64;
    let mut partial = 0u64;
    let mut blocks = 0u64;
    loop {
        if let Some(c) = o.count
            && blocks >= c
        {
            break;
        }
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        blocks += 1;
        read_total += n as u64;
        if n as u64 == o.bs {
            full += 1;
        } else {
            partial += 1;
        }
        output.write_all(&buf[..n])?;
        write_total += n as u64;
    }
    output.flush()?;
    Ok((read_total, write_total, full, partial))
}

impl Applet for Dd {
    fn name(&self) -> &'static str {
        "dd"
    }
    fn help(&self) -> &'static str {
        "dd [if=FILE] [of=FILE] [bs=N] [count=N] [skip=N] [seek=N] - copy data"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let opts = match parse_args(args) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("dd: {}", e);
                return ExitCode::FAILURE;
            }
        };
        match run_dd(&opts) {
            Ok((r, w, full, partial)) => {
                if !opts.quiet {
                    eprintln!("{} records in", full + partial);
                    eprintln!("{} records out", full + partial);
                    eprintln!("{} bytes copied (read {} bytes)", w, r);
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("dd: {}", e);
                ExitCode::FAILURE
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(DD.name(), "dd");
        assert!(DD.help().contains("copy"));
    }

    #[test]
    fn size_parsing() {
        assert_eq!(parse_size("512"), Some(512));
        assert_eq!(parse_size("1k"), Some(1024));
        assert_eq!(parse_size("2M"), Some(2 * 1024 * 1024));
        assert_eq!(parse_size("bad"), None);
    }

    #[test]
    fn parse_operands() {
        let o = parse_args(&[
            "if=/tmp/a".to_string(),
            "of=/tmp/b".to_string(),
            "bs=1k".to_string(),
            "count=3".to_string(),
        ])
        .unwrap();
        assert_eq!(o.ifile.as_deref(), Some("/tmp/a"));
        assert_eq!(o.bs, 1024);
        assert_eq!(o.count, Some(3));
        assert!(parse_args(&["bogus".to_string()]).is_err());
        assert!(parse_args(&["bs=0".to_string()]).is_err());
    }

    #[test]
    fn copies_file_with_count() {
        let src = format!("/tmp/rbox_dd_src_{}", std::process::id());
        let dst = format!("/tmp/rbox_dd_dst_{}", std::process::id());
        std::fs::write(&src, b"0123456789").unwrap();
        let o = DdOpts {
            ifile: Some(src.clone()),
            ofile: Some(dst.clone()),
            bs: 4,
            count: Some(2),
            ..Default::default()
        };
        let (r, w, full, partial) = run_dd(&o).unwrap();
        assert_eq!((r, w, full, partial), (8, 8, 2, 0));
        assert_eq!(std::fs::read(&dst).unwrap(), b"01234567");
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&dst);
    }
}
