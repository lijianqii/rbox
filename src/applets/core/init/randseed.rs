//! 随机种子持久化：启动时把上次保存的种子写入 /dev/urandom，关机时保存新种子。
//! 早期启动熵不足时（crng 未初始化）能显著加快随机数就绪。

/// 缺省种子文件路径。
pub(crate) const SEED_PATH: &str = "/var/lib/rbox/random-seed";
/// 种子长度（字节）。
const SEED_LEN: usize = 512;

/// 启动时恢复种子（文件缺失/损坏静默跳过）。
pub(crate) fn load() {
    let _ = load_from(SEED_PATH);
}

/// 关机时保存种子（失败仅告警）。
pub(crate) fn save() {
    if let Err(e) = save_to(SEED_PATH) {
        crate::applets::core::log_at(
            crate::applets::core::LogLevel::Warn,
            &format!("rbox init: failed to save random seed: {}", e),
        );
    }
}

/// 从指定路径恢复种子到 /dev/urandom。
pub(crate) fn load_from(path: &str) -> std::io::Result<()> {
    use std::io::{Read, Write};
    let mut seed = Vec::new();
    std::fs::File::open(path)?.read_to_end(&mut seed)?;
    if seed.is_empty() {
        return Ok(());
    }
    let mut urandom = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/urandom")?;
    urandom.write_all(&seed)?;
    Ok(())
}

/// 保存 /dev/urandom 的随机字节到指定路径（0600）。
pub(crate) fn save_to(path: &str) -> std::io::Result<()> {
    use std::io::{Read, Write};
    use std::os::unix::fs::OpenOptionsExt;
    let mut buf = [0u8; SEED_LEN];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(&buf)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load_roundtrip() {
        let path = format!("/tmp/rbox_seed_test_{}", std::process::id());
        let _ = std::fs::remove_file(&path);
        save_to(&path).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.len(), SEED_LEN as u64);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        // 恢复（写入 /dev/urandom）应成功
        assert!(load_from(&path).is_ok());
        let _ = std::fs::remove_file(&path);
        // 缺失文件：load 报错、save 正常
        assert!(load_from("/nonexistent/rbox-seed").is_err());
    }
}
