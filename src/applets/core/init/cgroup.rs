//! cgroup v2 资源限制：Slice/MemoryMax/CPUQuota/TasksMax。
//!
//! 启动时确保 cgroup2 挂载在 /sys/fs/cgroup；带资源限制的单元在启动后创建
//! 子 cgroup 并写入 memory.max / cpu.max / pids.max，再把服务进程加入。
//! KillMode=control-group 时优先用 cgroup.kill 整组终止。

use crate::applets::core::init::units::Unit;
use crate::applets::core::{LogLevel, log_at};

/// cgroup2 挂载点。
pub(crate) const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// 确保 cgroup2 已挂载（幂等）；成功返回 true。
pub(crate) fn ensure_mounted() -> bool {
    if std::path::Path::new(CGROUP_ROOT)
        .join("cgroup.controllers")
        .exists()
    {
        return true;
    }
    if std::fs::create_dir_all(CGROUP_ROOT).is_err() {
        return false;
    }
    let src = std::ffi::CString::new("none").unwrap();
    let target = std::ffi::CString::new(CGROUP_ROOT).unwrap();
    let fstype = std::ffi::CString::new("cgroup2").unwrap();
    let rc = unsafe {
        libc::mount(
            src.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            0,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        log_at(
            LogLevel::Warn,
            "rbox init: cgroup2 mount failed, resource limits disabled",
        );
        return false;
    }
    // 启用常用控制器（memory/pids/cpu）
    let _ = std::fs::write(
        format!("{}/cgroup.subtree_control", CGROUP_ROOT),
        "+memory +pids +cpu",
    );
    true
}

/// 解析字节大小：`64M` / `1G` / `512K` / 纯数字。
pub(crate) fn parse_bytes(spec: &str) -> Option<u64> {
    let s = spec.trim();
    if s.is_empty() || s == "infinity" {
        return None;
    }
    let (num, mul) = if let Some(n) = s.strip_suffix(['M', 'm']) {
        (n, 1024 * 1024)
    } else if let Some(n) = s.strip_suffix(['G', 'g']) {
        (n, 1024 * 1024 * 1024)
    } else if let Some(n) = s.strip_suffix(['K', 'k']) {
        (n, 1024)
    } else {
        (s, 1)
    };
    num.trim().parse::<u64>().ok().map(|v| v * mul)
}

/// 解析 CPUQuota（`50%` → `50000 100000`）。
pub(crate) fn parse_cpu_quota(spec: &str) -> Option<String> {
    let s = spec.trim();
    let pct = s.strip_suffix('%')?.trim().parse::<u64>().ok()?;
    if pct == 0 {
        return None;
    }
    Some(format!("{} 100000", pct * 1000))
}

/// 单元是否需要 cgroup 资源限制。
pub(crate) fn has_limits(unit: &Unit) -> bool {
    unit.service.memory_max.is_some()
        || unit.service.cpu_quota.is_some()
        || unit.service.cpu_weight.is_some()
        || unit.service.tasks_max.is_some()
        || unit.service.slice.is_some()
}

/// 为服务创建 cgroup 并写入限制，返回 cgroup 路径（失败返回 None）。
pub(crate) fn setup(unit: &Unit, pid: u32) -> Option<String> {
    if !has_limits(unit) {
        return None;
    }
    let slice = unit.service.slice.as_deref().unwrap_or("system.slice");
    let slice_path = format!("{}/{}", CGROUP_ROOT, slice);
    if std::fs::create_dir_all(&slice_path).is_err() {
        return None;
    }
    // 在 slice 上启用控制器，子 cgroup 才会出现 memory.max/cpu.max/pids.max
    let _ = std::fs::write(
        format!("{}/cgroup.subtree_control", slice_path),
        "+memory +pids +cpu",
    );
    let path = format!("{}/{}", slice_path, unit.name);
    if std::fs::create_dir_all(&path).is_err() {
        return None;
    }
    if let Some(m) = unit.service.memory_max.as_deref().and_then(parse_bytes) {
        let _ = std::fs::write(format!("{}/memory.max", path), m.to_string());
    }
    if let Some(q) = unit.service.cpu_quota.as_deref().and_then(parse_cpu_quota) {
        // cpu.max 需要内核 CONFIG_CFS_BANDWIDTH；缺失时告警并跳过
        if std::path::Path::new(&format!("{}/cpu.max", path)).exists() {
            let _ = std::fs::write(format!("{}/cpu.max", path), q);
        } else {
            log_at(
                LogLevel::Warn,
                &format!(
                    "rbox init: {} CPUQuota ignored (kernel lacks CONFIG_CFS_BANDWIDTH)",
                    unit.name
                ),
            );
        }
    }
    if let Some(w) = unit.service.cpu_weight {
        let _ = std::fs::write(format!("{}/cpu.weight", path), w.to_string());
    }
    if let Some(t) = unit.service.tasks_max {
        let _ = std::fs::write(format!("{}/pids.max", path), t.to_string());
    }
    if std::fs::write(format!("{}/cgroup.procs", path), pid.to_string()).is_err() {
        return None;
    }
    Some(path)
}

/// 整组终止（cgroup.kill，内核 5.14+）。
pub(crate) fn kill(path: &str) -> bool {
    std::fs::write(format!("{}/cgroup.kill", path), "1").is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_parsing() {
        assert_eq!(parse_bytes("64M"), Some(64 * 1024 * 1024));
        assert_eq!(parse_bytes("1G"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_bytes("512K"), Some(512 * 1024));
        assert_eq!(parse_bytes("1024"), Some(1024));
        assert_eq!(parse_bytes("infinity"), None);
        assert_eq!(parse_bytes("bad"), None);
    }

    #[test]
    fn cpu_quota_parsing() {
        assert_eq!(parse_cpu_quota("50%").as_deref(), Some("50000 100000"));
        assert_eq!(parse_cpu_quota("100%").as_deref(), Some("100000 100000"));
        assert_eq!(parse_cpu_quota("0%"), None);
        assert_eq!(parse_cpu_quota("50"), None);
    }
}
