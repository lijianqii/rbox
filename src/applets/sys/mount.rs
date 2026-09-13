//! `mount` - 挂载文件系统。
//!
//! 用法：mount                                # 列出当前挂载（/proc/mounts）
//!       mount [-t TYPE] [-o OPTS] DEVICE DIR
//!       mount [-t TYPE] [-o OPTS] TARGET     # TARGET 查 /etc/fstab
//!
//! `-o` 支持常见标志（ro/rw/nosuid/nodev/noexec/sync/noatime/remount 等），
//! 未知选项作为 data 传给内核（如 size=64m、mode=0755）。`-r`/`-w` 为
//! `-o ro`/`-o rw` 的简写。仅给一个目标时按 /etc/fstab 的挂载点或设备查找。

use crate::applet::Applet;
use crate::applets::fstab::{find_entry, parse_fstab};
use std::ffi::CString;
use std::process::ExitCode;

pub struct Mount;
pub static MOUNT: &Mount = &Mount;

/// 解析后的挂载计划。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MountPlan {
    pub(crate) device: String,
    pub(crate) dir: String,
    pub(crate) fstype: String,
    pub(crate) options: String,
}

/// 合并 fstab 选项与命令行 -o 选项（去重 defaults）。
fn merge_options(base: &str, extra: &str) -> String {
    if extra.is_empty() {
        return base.to_string();
    }
    if base.is_empty() || base == "defaults" {
        return extra.to_string();
    }
    format!("{},{}", base, extra)
}

/// 由命令行参数与 fstab 内容生成挂载计划（纯函数，便于单测）。
pub(crate) fn plan_mount(
    fstab_content: &str,
    positional: &[String],
    fstype: Option<&str>,
    options: &str,
) -> Result<MountPlan, String> {
    match positional.len() {
        0 => Err("missing device and directory".to_string()),
        1 => {
            let key = &positional[0];
            let entries = parse_fstab(fstab_content);
            let e = find_entry(&entries, key)
                .ok_or_else(|| format!("can't find {} in /etc/fstab", key))?;
            Ok(MountPlan {
                device: e.device.clone(),
                dir: e.mountpoint.clone(),
                fstype: fstype.unwrap_or(&e.fstype).to_string(),
                options: merge_options(&e.options, options),
            })
        }
        2 => Ok(MountPlan {
            device: positional[0].clone(),
            dir: positional[1].clone(),
            fstype: fstype.unwrap_or("").to_string(),
            options: options.to_string(),
        }),
        _ => Err("too many arguments".to_string()),
    }
}

/// 解析 -o 选项：返回 (mount 标志位, 未识别选项拼成的 data 串)。
pub(crate) fn parse_mount_options(options: &str) -> (libc::c_ulong, String) {
    let mut flags: libc::c_ulong = 0;
    let mut data: Vec<&str> = Vec::new();
    for opt in options.split(',') {
        match opt {
            "" | "defaults" | "rw" | "dev" | "suid" | "exec" | "async" | "atime" => {}
            "ro" => flags |= libc::MS_RDONLY,
            "remount" => flags |= libc::MS_REMOUNT,
            "noexec" => flags |= libc::MS_NOEXEC,
            "nosuid" => flags |= libc::MS_NOSUID,
            "nodev" => flags |= libc::MS_NODEV,
            "noatime" => flags |= libc::MS_NOATIME,
            "sync" => flags |= libc::MS_SYNCHRONOUS,
            "bind" => flags |= libc::MS_BIND,
            "rbind" => flags |= libc::MS_BIND | libc::MS_REC,
            "move" => flags |= libc::MS_MOVE,
            "rec" => flags |= libc::MS_REC,
            "silent" => flags |= libc::MS_SILENT,
            "relatime" => flags |= libc::MS_RELATIME,
            "strictatime" => flags |= libc::MS_STRICTATIME,
            "lazytime" => flags |= libc::MS_LAZYTIME,
            other => data.push(other),
        }
    }
    (flags, data.join(","))
}

/// 调用 mount(2)。
pub(crate) fn do_mount(
    device: &str,
    dir: &str,
    fstype: &str,
    flags: libc::c_ulong,
    data: &str,
) -> std::io::Result<()> {
    let src = CString::new(device)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let tgt =
        CString::new(dir).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let fst = CString::new(fstype)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let dat =
        CString::new(data).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let data_ptr = if data.is_empty() {
        std::ptr::null::<std::ffi::c_void>()
    } else {
        dat.as_ptr() as *const std::ffi::c_void
    };
    let rc = unsafe { libc::mount(src.as_ptr(), tgt.as_ptr(), fst.as_ptr(), flags, data_ptr) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// 从 /proc/filesystems 读取可尝试的文件系统类型列表（去掉 nodev 前缀）。
pub(crate) fn filesystem_types(content: &str) -> Vec<String> {
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            l.strip_prefix("nodev")
                .map(str::trim)
                .unwrap_or(l)
                .to_string()
        })
        .collect()
}

/// 列出当前挂载（/proc/mounts 原样输出）。
fn list_mounts() -> ExitCode {
    let path = format!("{}/mounts", crate::config::load().paths.proc);
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            print!("{}", content);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("mount: cannot read {}: {}", path, e);
            ExitCode::FAILURE
        }
    }
}

impl Applet for Mount {
    fn name(&self) -> &'static str {
        "mount"
    }
    fn help(&self) -> &'static str {
        "mount [-t TYPE] [-o OPTS] [DEVICE DIR|TARGET] - mount a filesystem (no args: list)"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut fstype: Option<String> = None;
        let mut options = String::new();
        let mut positional: Vec<String> = Vec::new();
        let mut end_of_options = false;
        let mut i = 0;
        while i < args.len() {
            let a = args[i].as_str();
            if end_of_options {
                positional.push(a.to_string());
                i += 1;
                continue;
            }
            match a {
                "--" => end_of_options = true,
                "-t" => {
                    i += 1;
                    let Some(t) = args.get(i) else {
                        eprintln!("mount: option -t requires an argument");
                        return ExitCode::FAILURE;
                    };
                    fstype = Some(t.clone());
                }
                "-o" => {
                    i += 1;
                    let Some(o) = args.get(i) else {
                        eprintln!("mount: option -o requires an argument");
                        return ExitCode::FAILURE;
                    };
                    options = merge_options(&options, o);
                }
                "-r" | "--read-only" => options = merge_options(&options, "ro"),
                "-w" | "--rw" => options = merge_options(&options, "rw"),
                s if s.starts_with('-') && s.len() > 1 => {
                    eprintln!("mount: unknown option: {}", s);
                    return ExitCode::FAILURE;
                }
                _ => positional.push(a.to_string()),
            }
            i += 1;
        }

        if positional.is_empty() {
            return list_mounts();
        }

        let fstab_content =
            std::fs::read_to_string(&crate::config::load().paths.fstab).unwrap_or_default();
        let plan = match plan_mount(&fstab_content, &positional, fstype.as_deref(), &options) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("mount: {}", e);
                return ExitCode::FAILURE;
            }
        };
        let (flags, data) = parse_mount_options(&plan.options);

        // 未指定类型：按 /proc/filesystems 顺序尝试自动探测
        if plan.fstype.is_empty() {
            let content = std::fs::read_to_string("/proc/filesystems").unwrap_or_default();
            let types = filesystem_types(&content);
            let mut last_err = None;
            for t in &types {
                match do_mount(&plan.device, &plan.dir, t, flags, &data) {
                    Ok(()) => return ExitCode::SUCCESS,
                    Err(e) => last_err = Some(e),
                }
            }
            eprintln!(
                "mount: mounting {} on {} failed: {}",
                plan.device,
                plan.dir,
                last_err
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "no filesystem type available".to_string())
            );
            return ExitCode::FAILURE;
        }

        match do_mount(&plan.device, &plan.dir, &plan.fstype, flags, &data) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!(
                    "mount: mounting {} on {} failed: {}",
                    plan.device, plan.dir, e
                );
                ExitCode::FAILURE
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FSTAB: &str = "\
proc     /proc      proc      defaults  0 0
tmpfs    /tmp       tmpfs     size=64m  0 0
";

    #[test]
    fn name_and_help() {
        assert_eq!(MOUNT.name(), "mount");
        assert!(MOUNT.help().contains("mount"));
    }

    #[test]
    fn option_flags_mapping() {
        let (flags, data) = parse_mount_options("ro,nosuid,nodev,noexec");
        assert_ne!(flags & libc::MS_RDONLY, 0);
        assert_ne!(flags & libc::MS_NOSUID, 0);
        assert_ne!(flags & libc::MS_NODEV, 0);
        assert_ne!(flags & libc::MS_NOEXEC, 0);
        assert_eq!(data, "");
    }

    #[test]
    fn unknown_options_go_to_data() {
        let (flags, data) = parse_mount_options("size=64m,mode=0755,rw");
        assert_eq!(flags & libc::MS_RDONLY, 0);
        assert_eq!(data, "size=64m,mode=0755");
    }

    #[test]
    fn filesystem_types_strips_nodev() {
        let types = filesystem_types("nodev\tsysfs\nnodev\tproc\n\text4\n");
        assert_eq!(types, vec!["sysfs", "proc", "ext4"]);
    }

    #[test]
    fn plan_from_fstab_by_mountpoint() {
        let plan = plan_mount(FSTAB, &["/tmp".to_string()], None, "").unwrap();
        assert_eq!(plan.device, "tmpfs");
        assert_eq!(plan.dir, "/tmp");
        assert_eq!(plan.fstype, "tmpfs");
        assert_eq!(plan.options, "size=64m");
    }

    #[test]
    fn plan_from_fstab_by_device_and_merges_options() {
        let plan = plan_mount(FSTAB, &["proc".to_string()], None, "ro,nosuid").unwrap();
        assert_eq!(plan.dir, "/proc");
        // fstab defaults + 命令行选项 -> 仅保留命令行
        assert_eq!(plan.options, "ro,nosuid");
    }

    #[test]
    fn plan_from_fstab_type_override() {
        let plan = plan_mount(FSTAB, &["/tmp".to_string()], Some("tmpfs"), "").unwrap();
        assert_eq!(plan.fstype, "tmpfs");
    }

    #[test]
    fn plan_explicit_device_dir() {
        let plan = plan_mount(
            "",
            &["/dev/vda".to_string(), "/mnt".to_string()],
            None,
            "ro",
        )
        .unwrap();
        assert_eq!(plan.device, "/dev/vda");
        assert_eq!(plan.dir, "/mnt");
        assert_eq!(plan.fstype, "");
        assert_eq!(plan.options, "ro");
    }

    #[test]
    fn plan_missing_fstab_entry_errors() {
        let err = plan_mount(FSTAB, &["/nope".to_string()], None, "").unwrap_err();
        assert!(err.contains("/etc/fstab"), "{}", err);
    }

    #[test]
    fn list_without_args_ok() {
        // /proc 在宿主机存在；仅验证不 panic 且返回成功
        assert_eq!(MOUNT.run(&[]), ExitCode::SUCCESS);
    }

    #[test]
    fn invalid_option_fails() {
        assert_eq!(MOUNT.run(&["-x".to_string()]), ExitCode::FAILURE);
    }

    #[test]
    fn wrong_operand_count_fails() {
        assert_eq!(MOUNT.run(&["/dev/x".to_string()]), ExitCode::FAILURE);
        assert_eq!(
            MOUNT.run(&[
                "/dev/x".to_string(),
                "/mnt".to_string(),
                "extra".to_string()
            ]),
            ExitCode::FAILURE
        );
    }
}
