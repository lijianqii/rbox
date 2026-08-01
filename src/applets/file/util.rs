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
