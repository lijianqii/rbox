//! 文件操作共享工具。
use std::fs;
use std::io;
use std::path::Path;

/// 递归删除文件或目录（rm -r 与跨文件系统 mv 共用）。
pub(crate) fn remove_recursive(path: &str) -> io::Result<()> {
    let meta = fs::metadata(path)?;
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let name = entry.file_name();
            let sub = Path::new(path).join(&name);
            remove_recursive(&sub.to_string_lossy())?;
        }
        fs::remove_dir(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// 判断路径是否为目录（不存在则 false）。
pub(crate) fn is_dir(path: &str) -> bool {
    fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
}

/// 若目标是目录，拼接 `dest/src_basename`；否则直接返回 `dest`。
pub(crate) fn resolve_dest(src: &str, dest: &str) -> io::Result<std::path::PathBuf> {
    if is_dir(dest) {
        let name = Path::new(src)
            .file_name()
            .ok_or_else(|| io::Error::other("cannot determine filename"))?;
        Ok(Path::new(dest).join(name))
    } else {
        Ok(Path::new(dest).to_path_buf())
    }
}

/// 简单递归复制（跨文件系统 mv 用）。
pub(crate) fn copy_recursive(src: &str, dest: &std::path::Path) -> io::Result<()> {
    let meta = fs::metadata(src)?;
    if meta.is_dir() {
        fs::create_dir_all(dest)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let name = entry.file_name();
            copy_recursive(
                &Path::new(src).join(&name).to_string_lossy(),
                &dest.join(&name),
            )?;
        }
    } else {
        let mut f_in = fs::File::open(src)?;
        let mut f_out = fs::File::create(dest)?;
        io::copy(&mut f_in, &mut f_out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn remove_file_simple() {
        let dir = format!("/tmp/rbox_test_{}", std::process::id());
        let _ = fs::create_dir_all(&dir);
        let f = format!("{}/testfile", dir);
        fs::write(&f, "hello").unwrap();
        assert!(Path::new(&f).exists());
        remove_recursive(&f).unwrap();
        assert!(!Path::new(&f).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_dir_recursive() {
        let dir = format!("/tmp/rbox_test_r_{}", std::process::id());
        let _ = fs::create_dir_all(&dir);
        fs::create_dir(format!("{}/sub", dir)).unwrap();
        fs::write(format!("{}/sub/a.txt", dir), "a").unwrap();
        fs::write(format!("{}/sub/b.txt", dir), "b").unwrap();
        remove_recursive(&format!("{}/sub", dir)).unwrap();
        assert!(!Path::new(&format!("{}/sub", dir)).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_dir_check() {
        let dir = format!("/tmp/rbox_test_d_{}", std::process::id());
        let _ = fs::create_dir_all(&dir);
        assert!(is_dir(&dir));
        assert!(!is_dir(&format!("{}/nonexistent", dir)));
        let f = format!("{}/file", dir);
        fs::write(&f, "x").unwrap();
        assert!(!is_dir(&f));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_dest_to_dir() {
        let dir = format!("/tmp/rbox_test_rd_{}", std::process::id());
        let _ = fs::create_dir_all(&dir);
        let dest = resolve_dest("/path/to/file.txt", &dir).unwrap();
        assert_eq!(dest, Path::new(&dir).join("file.txt"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_dest_to_file() {
        let dir = format!("/tmp/rbox_test_rf_{}", std::process::id());
        let _ = fs::create_dir_all(&dir);
        let dest = format!("{}/output", dir);
        fs::write(&dest, "x").unwrap();
        let result = resolve_dest("/path/to/src.txt", &dest).unwrap();
        assert_eq!(result, Path::new(&dest).to_path_buf());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_recursive_file() {
        let dir = format!("/tmp/rbox_test_cp_{}", std::process::id());
        let _ = fs::create_dir_all(&dir);
        let src = format!("{}/src.txt", dir);
        let dst = format!("{}/dst.txt", dir);
        fs::write(&src, "hello world").unwrap();
        copy_recursive(&src, Path::new(&dst)).unwrap();
        assert_eq!(fs::read_to_string(&dst).unwrap(), "hello world");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_recursive_directory() {
        let dir = format!("/tmp/rbox_test_cpd_{}", std::process::id());
        let _ = fs::create_dir_all(&dir);
        let srcdir = format!("{}/srcdir", dir);
        let _ = fs::create_dir_all(&srcdir);
        fs::write(format!("{}/srcdir/a.txt", dir), "a").unwrap();
        fs::write(format!("{}/srcdir/b.txt", dir), "b").unwrap();
        let dstdir = format!("{}/dstdir", dir);
        copy_recursive(&srcdir, Path::new(&dstdir)).unwrap();
        assert_eq!(
            fs::read_to_string(format!("{}/dstdir/a.txt", dir)).unwrap(),
            "a"
        );
        assert_eq!(
            fs::read_to_string(format!("{}/dstdir/b.txt", dir)).unwrap(),
            "b"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
