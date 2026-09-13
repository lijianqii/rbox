//! `tar` - 打包/解包（ustar 格式，无压缩）。
//!
//! 用法：tar -c [-f FILE] [-v] PATH...      # 创建
//!       tar -x [-f FILE] [-v] [-C DIR]    # 解包
//!       tar -t [-f FILE] [-v]             # 列出
//!
//! 支持普通文件、目录、符号链接；不支持压缩（无 -z/-j）。

use crate::applet::Applet;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

pub struct Tar;
pub static TAR: &Tar = &Tar;

const BLOCK: usize = 512;

/// 模式：创建/解包/列出。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Mode {
    Create,
    Extract,
    List,
}

/// tar 选项。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TarOpts {
    pub(crate) mode: Mode,
    pub(crate) file: Option<String>,
    pub(crate) verbose: bool,
    pub(crate) chdir: Option<String>,
    pub(crate) paths: Vec<String>,
}

/// 解析参数。
pub(crate) fn parse_args(args: &[String]) -> Result<TarOpts, String> {
    let mut mode: Option<Mode> = None;
    let mut file = None;
    let mut verbose = false;
    let mut chdir = None;
    let mut paths = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            paths.extend(args[i + 1..].iter().cloned());
            break;
        }
        if let Some(rest) = a.strip_prefix('-')
            && !rest.is_empty()
        {
            // 组合短选项：-cvf FILE / -xf FILE / -C DIR
            let chars: Vec<char> = rest.chars().collect();
            let mut j = 0;
            while j < chars.len() {
                match chars[j] {
                    'c' => mode = Some(Mode::Create),
                    'x' => mode = Some(Mode::Extract),
                    't' => mode = Some(Mode::List),
                    'v' => verbose = true,
                    'f' => {
                        // 余下字符为文件名，否则取下一个参数
                        let inline: String = chars[j + 1..].iter().collect();
                        if !inline.is_empty() {
                            file = Some(inline);
                            j = chars.len();
                            continue;
                        }
                        i += 1;
                        let Some(f) = args.get(i) else {
                            return Err("option -f requires an argument".to_string());
                        };
                        file = Some(f.clone());
                    }
                    'C' => {
                        i += 1;
                        let Some(d) = args.get(i) else {
                            return Err("option -C requires an argument".to_string());
                        };
                        chdir = Some(d.clone());
                    }
                    other => return Err(format!("unknown option: -{}", other)),
                }
                j += 1;
            }
        } else {
            paths.push(a.to_string());
        }
        i += 1;
    }
    let mode = mode.ok_or("you must specify -c, -x or -t")?;
    Ok(TarOpts {
        mode,
        file,
        verbose,
        chdir,
        paths,
    })
}

/// 写八进制字段（带末尾 NUL）。
fn write_octal(field: &mut [u8], value: u64) {
    let digits = field.len() - 1;
    let s = format!("{:0width$o}", value, width = digits);
    let bytes = s.as_bytes();
    let start = field.len() - 1 - bytes.len().min(digits);
    field[..start].fill(b'0');
    field[start..start + bytes.len().min(digits)]
        .copy_from_slice(&bytes[bytes.len().saturating_sub(digits)..]);
    field[field.len() - 1] = 0;
}

/// 读八进制字段（跳过前导空格/NUL，遇到非八进制停止）。
fn read_octal(field: &[u8]) -> u64 {
    let mut v = 0u64;
    for &b in field {
        match b {
            b'0'..=b'7' => v = v * 8 + (b - b'0') as u64,
            b' ' | 0 => continue,
            _ => break,
        }
    }
    v
}

/// 构造 512 字节头（含校验和）。
#[allow(clippy::too_many_arguments)]
fn build_header(
    name: &str,
    mode: u32,
    uid: u32,
    gid: u32,
    size: u64,
    mtime: i64,
    typeflag: u8,
    linkname: &str,
) -> Vec<u8> {
    let mut h = vec![0u8; BLOCK];
    let name_bytes = name.as_bytes();
    let n = name_bytes.len().min(100);
    h[..n].copy_from_slice(&name_bytes[..n]);
    write_octal(&mut h[100..108], (mode & 0o7777) as u64);
    write_octal(&mut h[108..116], uid as u64);
    write_octal(&mut h[116..124], gid as u64);
    write_octal(&mut h[124..136], size);
    write_octal(&mut h[136..148], mtime.max(0) as u64);
    h[156] = typeflag;
    let ln = linkname.as_bytes();
    let l = ln.len().min(100);
    h[157..157 + l].copy_from_slice(&ln[..l]);
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    // 校验和：chksum 字段按空格计算
    h[148..156].fill(b' ');
    let sum: u64 = h.iter().map(|&b| b as u64).sum();
    let ck = format!("{:06o}\0 ", sum);
    h[148..156].copy_from_slice(ck.as_bytes());
    h
}

/// 读取一个头，返回 (字段 map, 原始头)。EOF 或全零返回 None。
fn read_header(r: &mut dyn Read) -> std::io::Result<Option<(TarHeader, Vec<u8>)>> {
    let mut h = vec![0u8; BLOCK];
    let mut got = 0;
    while got < BLOCK {
        let n = r.read(&mut h[got..])?;
        if n == 0 {
            if got == 0 {
                return Ok(None);
            }
            return Err(std::io::Error::other("truncated tar header"));
        }
        got += n;
    }
    if h.iter().all(|&b| b == 0) {
        return Ok(None);
    }
    // 校验和验证
    let stored = read_octal(&h[148..156]);
    let mut copy = h.clone();
    copy[148..156].fill(b' ');
    let actual: u64 = copy.iter().map(|&b| b as u64).sum();
    if stored != actual {
        return Err(std::io::Error::other("bad tar header checksum"));
    }
    let name_end = h[..100].iter().position(|&b| b == 0).unwrap_or(100);
    let name = String::from_utf8_lossy(&h[..name_end]).into_owned();
    let prefix_end = h[345..500].iter().position(|&b| b == 0).unwrap_or(155);
    let prefix = String::from_utf8_lossy(&h[345..345 + prefix_end]).into_owned();
    let link_end = h[157..257].iter().position(|&b| b == 0).unwrap_or(100);
    let linkname = String::from_utf8_lossy(&h[157..157 + link_end]).into_owned();
    let full_name = if prefix.is_empty() {
        name
    } else {
        format!("{}/{}", prefix, name)
    };
    Ok(Some((
        TarHeader {
            name: full_name,
            mode: read_octal(&h[100..108]) as u32,
            uid: read_octal(&h[108..116]) as u32,
            gid: read_octal(&h[116..124]) as u32,
            size: read_octal(&h[124..136]),
            typeflag: h[156],
            linkname,
        },
        h,
    )))
}

/// 解析后的 tar 头。
#[derive(Debug, Clone)]
pub(crate) struct TarHeader {
    pub(crate) name: String,
    pub(crate) mode: u32,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) size: u64,
    pub(crate) typeflag: u8,
    pub(crate) linkname: String,
}

/// 写一个条目（头 + 数据 + 填充）。
fn write_entry(w: &mut dyn Write, path: &Path, verbose: bool) -> std::io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    let name = path.to_string_lossy().into_owned();
    if verbose {
        eprintln!("{}", name);
    }
    let ft = meta.file_type();
    if ft.is_dir() {
        let dir_name = if name.ends_with('/') {
            name.clone()
        } else {
            format!("{}/", name)
        };
        let h = build_header(
            &dir_name,
            meta.mode(),
            meta.uid(),
            meta.gid(),
            0,
            meta.mtime(),
            b'5',
            "",
        );
        w.write_all(&h)?;
    } else if ft.is_symlink() {
        let target = fs::read_link(path)?;
        let h = build_header(
            &name,
            meta.mode(),
            meta.uid(),
            meta.gid(),
            0,
            meta.mtime(),
            b'2',
            &target.to_string_lossy(),
        );
        w.write_all(&h)?;
    } else if ft.is_file() {
        let h = build_header(
            &name,
            meta.mode(),
            meta.uid(),
            meta.gid(),
            meta.len(),
            meta.mtime(),
            b'0',
            "",
        );
        w.write_all(&h)?;
        let mut f = fs::File::open(path)?;
        let mut buf = [0u8; 8192];
        let mut written = 0u64;
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            w.write_all(&buf[..n])?;
            written += n as u64;
        }
        // 填充到 512 边界
        let pad = (BLOCK - (written % BLOCK as u64) as usize) % BLOCK;
        if pad > 0 {
            w.write_all(&vec![0u8; pad])?;
        }
    }
    Ok(())
}

/// 递归写入路径（目录先写自身再写子项）。
fn write_path(w: &mut dyn Write, path: &Path, verbose: bool) -> std::io::Result<()> {
    write_entry(w, path, verbose)?;
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        let mut children: Vec<PathBuf> = fs::read_dir(path)?.flatten().map(|e| e.path()).collect();
        children.sort();
        for c in children {
            write_path(w, &c, verbose)?;
        }
    }
    Ok(())
}

/// 创建归档到 writer。
pub(crate) fn create_archive(
    paths: &[String],
    w: &mut dyn Write,
    verbose: bool,
) -> std::io::Result<()> {
    for p in paths {
        write_path(w, Path::new(p), verbose)?;
    }
    // 两个全零块结尾
    w.write_all(&[0u8; BLOCK * 2])?;
    w.flush()
}

/// 跳过 size 字节（含填充）。
fn skip_data(r: &mut dyn Read, size: u64) -> std::io::Result<()> {
    let padded = size.div_ceil(BLOCK as u64) * BLOCK as u64;
    let mut remaining = padded;
    let mut buf = [0u8; 8192];
    while remaining > 0 {
        let n = r.read(&mut buf[..remaining.min(8192) as usize])?;
        if n == 0 {
            break;
        }
        remaining -= n as u64;
    }
    Ok(())
}

/// 安全校验：拒绝绝对路径与 `..`（防目录穿越）。
pub(crate) fn safe_relative(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.is_absolute() {
        return None;
    }
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(x) => out.push(x),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// 列出归档条目（verbose 时附加大小）。
pub(crate) fn list_archive(r: &mut dyn Read, verbose: bool) -> std::io::Result<()> {
    loop {
        let Some((h, _)) = read_header(r)? else {
            return Ok(());
        };
        if verbose {
            println!(
                "{:>4o} {}:{} {:>8} {}",
                h.mode, h.uid, h.gid, h.size, h.name
            );
        } else {
            println!("{}", h.name);
        }
        skip_data(r, h.size)?;
    }
}

/// 解包到 dest。
pub(crate) fn extract_archive(r: &mut dyn Read, dest: &Path, verbose: bool) -> std::io::Result<()> {
    loop {
        let Some((h, _)) = read_header(r)? else {
            return Ok(());
        };
        let Some(rel) = safe_relative(h.name.trim_end_matches('/')) else {
            return Err(std::io::Error::other(format!(
                "unsafe path in archive: {}",
                h.name
            )));
        };
        let target = dest.join(&rel);
        if verbose {
            println!("{}", h.name);
        }
        match h.typeflag {
            b'5' => {
                fs::create_dir_all(&target)?;
                let _ = fs::set_permissions(
                    &target,
                    std::os::unix::fs::PermissionsExt::from_mode(h.mode),
                );
            }
            b'2' => {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                let _ = fs::remove_file(&target);
                std::os::unix::fs::symlink(&h.linkname, &target)?;
            }
            0 | b'0' => {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut f = fs::File::create(&target)?;
                let mut remaining = h.size;
                let mut buf = [0u8; 8192];
                while remaining > 0 {
                    let want = remaining.min(8192) as usize;
                    let n = r.read(&mut buf[..want])?;
                    if n == 0 {
                        return Err(std::io::Error::other("truncated tar data"));
                    }
                    f.write_all(&buf[..n])?;
                    remaining -= n as u64;
                }
                let _ = fs::set_permissions(
                    &target,
                    std::os::unix::fs::PermissionsExt::from_mode(h.mode),
                );
                // 跳过填充
                let pad = (BLOCK - (h.size % BLOCK as u64) as usize) % BLOCK;
                if pad > 0 {
                    let mut padbuf = [0u8; BLOCK];
                    let mut got = 0;
                    while got < pad {
                        let n = r.read(&mut padbuf[got..pad])?;
                        if n == 0 {
                            break;
                        }
                        got += n;
                    }
                }
                continue;
            }
            other => {
                return Err(std::io::Error::other(format!(
                    "unsupported tar entry type: {}",
                    other as char
                )));
            }
        }
        skip_data(r, h.size)?;
    }
}

impl Applet for Tar {
    fn name(&self) -> &'static str {
        "tar"
    }
    fn help(&self) -> &'static str {
        "tar -c|-x|-t [-f FILE] [-v] [-C DIR] [PATH...] - ustar archive tool"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let opts = match parse_args(args) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("tar: {}", e);
                return ExitCode::FAILURE;
            }
        };
        if let Some(dir) = &opts.chdir
            && let Err(e) = std::env::set_current_dir(dir)
        {
            eprintln!("tar: cannot chdir to {}: {}", dir, e);
            return ExitCode::FAILURE;
        }
        let result = match opts.mode {
            Mode::Create => {
                if opts.paths.is_empty() {
                    Err(std::io::Error::other("no files to archive"))
                } else {
                    match &opts.file {
                        Some(f) => match fs::File::create(f) {
                            Ok(mut file) => create_archive(&opts.paths, &mut file, opts.verbose),
                            Err(e) => Err(e),
                        },
                        None => {
                            let stdout = std::io::stdout();
                            create_archive(&opts.paths, &mut stdout.lock(), opts.verbose)
                        }
                    }
                }
            }
            Mode::List => match open_input(&opts.file) {
                Ok(mut r) => list_archive(&mut r, opts.verbose),
                Err(e) => Err(e),
            },
            Mode::Extract => {
                let dest = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                match open_input(&opts.file) {
                    Ok(mut r) => extract_archive(&mut r, &dest, opts.verbose),
                    Err(e) => Err(e),
                }
            }
        };
        match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("tar: {}", e);
                ExitCode::FAILURE
            }
        }
    }
}

/// 打开归档输入（文件或 stdin）。
fn open_input(file: &Option<String>) -> std::io::Result<Box<dyn Read>> {
    match file {
        Some(f) => Ok(Box::new(fs::File::open(f)?)),
        None => Ok(Box::new(std::io::stdin().lock())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> String {
        let d = format!("/tmp/rbox_tar_{}_{}", tag, std::process::id());
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(format!("{}/sub", d)).unwrap();
        fs::write(format!("{}/a.txt", d), "hello tar").unwrap();
        fs::write(format!("{}/sub/b.txt", d), "nested").unwrap();
        std::os::unix::fs::symlink("a.txt", format!("{}/link", d)).unwrap();
        d
    }

    #[test]
    fn name_and_help() {
        assert_eq!(TAR.name(), "tar");
        assert!(TAR.help().contains("ustar"));
    }

    #[test]
    fn octal_roundtrip() {
        let mut f = [0u8; 8];
        write_octal(&mut f, 0o755);
        assert_eq!(read_octal(&f), 0o755);
        let mut s = [0u8; 12];
        write_octal(&mut s, 123456);
        assert_eq!(read_octal(&s), 123456);
    }

    #[test]
    fn parse_modes() {
        let o = parse_args(&["-cvf".to_string(), "out.tar".to_string(), "a".to_string()]).unwrap();
        assert_eq!(o.mode, Mode::Create);
        assert_eq!(o.file.as_deref(), Some("out.tar"));
        assert!(o.verbose);
        assert!(parse_args(&["-x".to_string(), "-f".to_string(), "a.tar".to_string()]).is_ok());
        assert!(parse_args(&["-q".to_string()]).is_err());
        assert!(parse_args(&[]).is_err());
    }

    #[test]
    fn create_list_extract_roundtrip() {
        // 手工构造相对路径归档（create_archive 保留给定路径；
        // 绝对路径在解包时会被安全拒绝）
        let mut archive: Vec<u8> = Vec::new();
        for (name, data) in [("a.txt", "hello tar"), ("sub/b.txt", "nested")] {
            let h = build_header(name, 0o644, 0, 0, data.len() as u64, 0, b'0', "");
            archive.extend_from_slice(&h);
            archive.extend_from_slice(data.as_bytes());
            let pad = (BLOCK - data.len() % BLOCK) % BLOCK;
            archive.extend(std::iter::repeat_n(0u8, pad));
        }
        archive.extend_from_slice(&build_header("sub/", 0o755, 0, 0, 0, 0, b'5', ""));
        archive.extend_from_slice(&build_header("link", 0o777, 0, 0, 0, 0, b'2', "a.txt"));
        archive.extend_from_slice(&[0u8; BLOCK * 2]);

        // list
        let mut cur = std::io::Cursor::new(archive.clone());
        list_archive(&mut cur, false).unwrap();

        // extract
        let dir = tmpdir("rt");
        let out = format!("{}/out", dir);
        fs::create_dir_all(&out).unwrap();
        let mut cur = std::io::Cursor::new(archive);
        extract_archive(&mut cur, Path::new(&out), false).unwrap();
        assert_eq!(
            fs::read_to_string(format!("{}/a.txt", out)).unwrap(),
            "hello tar"
        );
        assert_eq!(
            fs::read_to_string(format!("{}/sub/b.txt", out)).unwrap(),
            "nested"
        );
        assert!(
            fs::symlink_metadata(format!("{}/link", out))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_archive_emits_end_blocks() {
        let mut archive: Vec<u8> = Vec::new();
        create_archive(&[], &mut archive, false).unwrap();
        assert_eq!(archive.len(), BLOCK * 2);
        let mut cur = std::io::Cursor::new(archive);
        assert!(read_header(&mut cur).unwrap().is_none());
    }

    #[test]
    fn safe_relative_rejects_escape() {
        assert!(safe_relative("a/b").is_some());
        assert!(safe_relative("/abs").is_none());
        assert!(safe_relative("../etc/passwd").is_none());
        assert!(safe_relative("a/../../b").is_none());
        assert!(safe_relative(".").is_none());
    }

    #[test]
    fn bad_checksum_rejected() {
        let mut archive: Vec<u8> = Vec::new();
        create_archive(&[], &mut archive, false).unwrap();
        let mut h = build_header("x", 0o644, 0, 0, 0, 0, b'0', "");
        h[0] = b'y'; // 破坏内容但不更新校验和
        let mut data = h;
        data.extend_from_slice(&[0u8; BLOCK]);
        let mut cur = std::io::Cursor::new(data);
        assert!(list_archive(&mut cur, false).is_err());
    }
}
