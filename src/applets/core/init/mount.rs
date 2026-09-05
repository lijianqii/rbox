//! 文件系统挂载（/etc/fstab）、主机名、sysctl 等系统初始化。

use crate::applets::core::log;
use std::fs;

/// 内置默认挂载集：/etc/fstab 缺失时回退使用。
const DEFAULT_FSTAB: &[&str] = &[
    "proc     /proc      proc      defaults  0 0",
    "sysfs    /sys       sysfs     defaults  0 0",
    "devtmpfs /dev       devtmpfs  defaults  0 0",
    "devpts   /dev/pts   devpts    defaults  0 0",
    "tmpfs    /tmp       tmpfs     defaults  0 0",
];

/// 一条 fstab 挂载记录：<device> <mountpoint> <type> <options> [<dump> <pass>]。
#[derive(Debug, Clone)]
struct FstabEntry {
    device: String,
    mountpoint: String,
    fstype: String,
    options: String,
}

/// 解析一行 fstab 记录；空行、注释行、字段不足的行返回 None。
fn parse_fstab_line(line: &str) -> Option<FstabEntry> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let mut fields = line.split_whitespace();
    let device = fields.next()?;
    let mountpoint = fields.next()?;
    let fstype = fields.next()?;
    let options = fields.next().unwrap_or("defaults");
    Some(FstabEntry {
        device: device.to_string(),
        mountpoint: mountpoint.to_string(),
        fstype: fstype.to_string(),
        options: options.to_string(),
    })
}

/// 解析整个 fstab 内容。
fn parse_fstab(content: &str) -> Vec<FstabEntry> {
    content.lines().filter_map(parse_fstab_line).collect()
}

/// 挂载所有文件系统：优先读取配置的 fstab，缺失时回退到内置默认集。
/// 单个挂载失败只记录日志，不中断其余挂载。
pub(crate) fn mount_all_fs() {
    let fstab_path = &crate::config::load().paths.fstab;
    let entries: Vec<FstabEntry> = match fs::read_to_string(fstab_path) {
        Ok(content) => parse_fstab(&content),
        Err(_) => {
            log("rbox init: /etc/fstab not found, using built-in defaults");
            DEFAULT_FSTAB
                .iter()
                .filter_map(|l| parse_fstab_line(l))
                .collect()
        }
    };
    for e in &entries {
        log(&format!(
            "rbox init: mounting {} on {} ({})",
            e.device, e.mountpoint, e.fstype
        ));
        let _ = fs::create_dir_all(&e.mountpoint);
        if let Err(err) = run_mount(&e.device, &e.mountpoint, &e.fstype, &e.options) {
            // EBUSY 且目标已以相同类型挂载（early 阶段预挂或内核自动挂载）
            // 属正常情况，不报错
            if err.raw_os_error() == Some(libc::EBUSY) && is_mounted_with(&e.mountpoint, &e.fstype)
            {
                continue;
            }
            log(&format!(
                "rbox init: mount {} on {} failed: {}",
                e.device, e.mountpoint, err
            ));
        }
    }
}

/// 检查 /proc/mounts 中 mountpoint 是否已以 fstype 挂载（EBUSY 时区分
/// “已挂载”与“挂载点被其他类型占用”两种情况）。
fn is_mounted_with(mountpoint: &str, fstype: &str) -> bool {
    let Ok(content) = fs::read_to_string("/proc/mounts") else {
        return false;
    };
    content
        .lines()
        .any(|l| mount_line_matches(l, mountpoint, fstype))
}

/// 判断一行挂载记录（/proc/mounts 格式）是否匹配指定的挂载点与文件系统类型。
fn mount_line_matches(line: &str, mountpoint: &str, fstype: &str) -> bool {
    let mut fields = line.split_whitespace();
    let _dev = fields.next();
    let mp = fields.next().unwrap_or("");
    let ft = fields.next().unwrap_or("");
    mp == mountpoint && ft == fstype
}

/// 为所有子进程（shell、服务）提供默认 PATH（可配置）。
pub(crate) fn setup_environment() {
    if std::env::var_os("PATH").is_none() {
        let path = crate::config::load().init.default_path.clone();
        // SAFETY: init 是单线程 PID 1，无并发修改环境变量风险
        unsafe { std::env::set_var("PATH", path) };
    }
}

/// 读取配置的主机名文件（取第一行）并设置主机名；文件缺失或为空时静默跳过。
pub(crate) fn setup_hostname() {
    let hostname_path = &crate::config::load().paths.hostname;
    let Ok(content) = fs::read_to_string(hostname_path) else {
        return;
    };
    let hostname = content.lines().next().unwrap_or("").trim();
    if hostname.is_empty() {
        return;
    }
    let Ok(c) = std::ffi::CString::new(hostname) else {
        return;
    };
    let rc = unsafe { libc::sethostname(c.as_ptr(), hostname.len()) };
    if rc == 0 {
        log(&format!("rbox init: hostname set to {}", hostname));
    } else {
        log(&format!(
            "rbox init: sethostname failed: {}",
            std::io::Error::last_os_error()
        ));
    }
}

/// 解析 sysctl.conf 内容为 (key, value) 对；跳过空行/注释/格式错误行。
fn parse_sysctl_conf(content: &str) -> Vec<(String, String)> {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            let value = value.trim();
            if key.is_empty() {
                return None;
            }
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

/// 应用 /etc/sysctl.conf：key 的点号转 /proc/sys/ 路径并写入。
/// 文件缺失时静默跳过；单个条目失败只记日志。
pub(crate) fn apply_sysctl(path: &str) {
    let Ok(content) = fs::read_to_string(path) else {
        return;
    };
    for (key, value) in parse_sysctl_conf(&content) {
        let proc_path = format!("/proc/sys/{}", key.replace('.', "/"));
        match fs::write(&proc_path, value.as_bytes()) {
            Ok(()) => log(&format!("rbox init: sysctl {} = {}", key, value)),
            Err(e) => log(&format!("rbox init: sysctl {} failed: {}", key, e)),
        }
    }
}

fn run_mount(src: &str, tgt: &str, fstype: &str, options: &str) -> std::io::Result<()> {
    use std::ffi::CString;
    let s =
        CString::new(src).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let t =
        CString::new(tgt).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let f = CString::new(fstype)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let rc = unsafe {
        libc::mount(
            s.as_ptr(),
            t.as_ptr(),
            f.as_ptr(),
            parse_mount_flags(options),
            std::ptr::null::<std::ffi::c_void>(),
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// 解析 fstab options（逗号分隔）为 mount(2) 标志位；defaults/未知选项视为 0。
fn parse_mount_flags(options: &str) -> libc::c_ulong {
    let mut flags: libc::c_ulong = 0;
    for opt in options.split(',') {
        match opt {
            "defaults" | "rw" | "" => {}
            "ro" => flags |= libc::MS_RDONLY,
            "remount" => flags |= libc::MS_REMOUNT,
            "noexec" => flags |= libc::MS_NOEXEC,
            "nosuid" => flags |= libc::MS_NOSUID,
            "nodev" => flags |= libc::MS_NODEV,
            "noatime" => flags |= libc::MS_NOATIME,
            "sync" => flags |= libc::MS_SYNCHRONOUS,
            _ => {}
        }
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fstab_basic_entries() {
        let entries = parse_fstab("proc /proc proc defaults 0 0\nsysfs /sys sysfs ro 0 0\n");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].device, "proc");
        assert_eq!(entries[0].mountpoint, "/proc");
        assert_eq!(entries[0].fstype, "proc");
        assert_eq!(entries[0].options, "defaults");
        assert_eq!(entries[1].options, "ro");
    }

    #[test]
    fn parse_fstab_skips_comments_and_blank_lines() {
        let entries = parse_fstab("# comment\n\n   \nproc /proc proc defaults 0 0\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].device, "proc");
    }

    #[test]
    fn parse_fstab_drops_short_lines() {
        let entries = parse_fstab("proc /proc\nproc /proc proc defaults 0 0\n");
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn parse_mount_flags_mapping() {
        assert_eq!(parse_mount_flags("defaults"), 0);
        assert_eq!(parse_mount_flags("rw"), 0);
        assert_eq!(parse_mount_flags("ro"), libc::MS_RDONLY);
        assert_eq!(
            parse_mount_flags("ro,noexec,nosuid"),
            libc::MS_RDONLY | libc::MS_NOEXEC | libc::MS_NOSUID
        );
        assert_eq!(parse_mount_flags("unknownopt"), 0);
    }

    #[test]
    fn mount_line_matches_basic() {
        assert!(mount_line_matches(
            "proc /proc proc rw,relatime 0 0",
            "/proc",
            "proc"
        ));
        assert!(!mount_line_matches(
            "proc /proc proc rw,relatime 0 0",
            "/proc",
            "sysfs"
        ));
        assert!(!mount_line_matches(
            "devtmpfs /dev devtmpfs rw 0 0",
            "/dev",
            "proc"
        ));
    }

    #[test]
    fn mount_line_matches_wrong_type() {
        // 挂载点相同但类型不同 → 不匹配
        assert!(!mount_line_matches(
            "/dev/vda /mnt ext4 rw 0 0",
            "/mnt",
            "tmpfs"
        ));
        // 类型相同但挂载点不同 → 不匹配
        assert!(!mount_line_matches(
            "tmpfs /run tmpfs rw 0 0",
            "/tmp",
            "tmpfs"
        ));
        // 字段不足的行 → 不匹配
        assert!(!mount_line_matches("proc /proc", "/proc", "proc"));
    }

    #[test]
    fn parse_sysctl_conf_basic() {
        let entries = parse_sysctl_conf(
            "# comment\n\nkernel.panic = 10\nnet.ipv4.ip_forward=1\nbroken-line\n",
        );
        assert_eq!(
            entries,
            vec![
                ("kernel.panic".to_string(), "10".to_string()),
                ("net.ipv4.ip_forward".to_string(), "1".to_string())
            ]
        );
    }
}
