//! `init` - PID 1 系统初始化进程。
//!
//! systemd 风格的 target 依赖链启动（配置使用 TOML 格式）：
//! - 解析 `/etc/rbox/system/` 下的 `.toml` 单元文件（units 模块）。
//! - 支持 `[Unit]` 的 `After=`/`Requires=`、`[Install]` 的 `WantedBy=`、
//!   `[Service]` 的 `Type=`(simple/forking)/`ExecStart=` 等（services 模块）。
//! - 从 `default.target` 出发，按依赖拓扑序启动服务。
//! - 启动完成后 fork 一个 shell（作为 getty 替代），init 作为 PID 1 常驻，
//!   回收僵尸/孤儿进程；shell 退出后重新 fork。
//! - 通过 unix socket 响应控制请求（server 模块：status/start/stop/restart/reload）。

pub(crate) mod mount;
pub(crate) mod server;
pub(crate) mod services;
pub(crate) mod syscall;
pub(crate) mod units;

use crate::applet::Applet;
use crate::applets::core::init::mount::{
    apply_sysctl, mount_all_fs, setup_environment, setup_hostname,
};
use crate::applets::core::init::server::{create_status_listener, handle_control_connection};
use crate::applets::core::init::services::{
    ServiceInstance, finish_daemonize, parse_environment, respawn_service, schedule_restart,
    spawn_fresh_shell, start_forking_service, start_service, stop_service_instance,
};
use crate::applets::core::init::syscall::{kill_all, reboot_syscall, sync_fs};
use crate::applets::core::init::units::{Unit, compute_start_order, load_all_units};
use crate::applets::core::{LogLevel, log, log_at};
use std::collections::HashMap;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

/// 全局关机标志：SIGTERM 信号处理器设置，主循环检查。
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
/// 全局重启标志：SIGINT 信号处理器设置，主循环检查。
static REBOOT_REQUESTED: AtomicBool = AtomicBool::new(false);
/// self-pipe 写端 fd：信号处理器写 1 字节唤醒主循环 poll；-1 表示未创建。
static SIGNAL_PIPE_WRITE: AtomicI32 = AtomicI32::new(-1);

/// 信号处理器：SIGTERM 设置关机标志，SIGINT 设置重启标志，SIGCHLD 仅唤醒；
/// 统一写 self-pipe 通知主循环（async-signal-safe：仅原子操作 + write）。
extern "C" fn signal_handler(sig: i32) {
    match sig {
        libc::SIGINT => REBOOT_REQUESTED.store(true, Ordering::SeqCst),
        libc::SIGTERM => SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst),
        _ => {} // SIGCHLD：仅唤醒主循环收割子进程
    }
    let fd = SIGNAL_PIPE_WRITE.load(Ordering::SeqCst);
    if fd >= 0 {
        let byte: u8 = 1;
        unsafe { libc::write(fd, &byte as *const u8 as *const libc::c_void, 1) };
    }
}

/// 是否已请求关机或重启。
pub(crate) fn shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::SeqCst) || REBOOT_REQUESTED.load(Ordering::SeqCst)
}

pub struct Init;
pub static INIT: &Init = &Init;

impl Applet for Init {
    fn name(&self) -> &'static str {
        "init"
    }
    fn help(&self) -> &'static str {
        "init [systemd-style] - PID 1 system initializer"
    }
    fn run(&self, _args: &[String]) -> ExitCode {
        let pid = std::process::id();
        let is_pid1 = pid == 1;

        if is_pid1 {
            log("rbox init: starting as PID 1");
            install_signal_handlers();
        } else {
            log("rbox init: running in test mode (not PID 1)");
        }

        // 早期根切换：内核指定 root= 且当前运行在 initramfs 时，
        // 挂载并切换到持久 rootfs（ext4），然后 exec 新根上的 init。
        if is_pid1 && early_root_handoff() {
            // 成功后进程被替换，不会返回
            return ExitCode::SUCCESS;
        }

        // 1. 基本环境与文件系统初始化（默认 PATH + /etc/fstab 挂载 + 主机名 + sysctl）
        setup_environment();
        mount_all_fs();
        setup_hostname();
        apply_sysctl(&crate::config::load().paths.sysctl_conf);
        log("rbox init: basic filesystems mounted");

        // 1.5 内核 cmdline 的 single/emergency：跳过单元加载，直接进应急/单用户 shell
        if is_pid1 {
            match boot_mode_from_cmdline() {
                BootMode::Emergency => return run_emergency_shell("emergency"),
                BootMode::Single => return run_emergency_shell("single"),
                BootMode::Normal => {}
            }
        }

        // 2. 解析所有单元文件
        let units = match load_all_units() {
            Ok(u) => {
                log(&format!("rbox init: loaded {} unit(s)", u.len()));
                u
            }
            Err(e) => {
                log_at(
                    LogLevel::Error,
                    &format!("rbox init: failed to load units: {}", e),
                );
                return run_emergency_shell("no units");
            }
        };

        // 3. 计算从 default.target 出发的启动顺序（拓扑排序；target 名可配置）
        let default_target = crate::config::load().paths.default_target.as_str();
        let order = match compute_start_order(&units, default_target) {
            Ok(o) => {
                log_at(LogLevel::Debug, &format!("rbox init: start order: {:?}", o));
                o
            }
            Err(e) => {
                log_at(
                    LogLevel::Error,
                    &format!("rbox init: dependency error: {}", e),
                );
                return run_emergency_shell("no units");
            }
        };

        // 4. 按依赖深度分层启动服务（同层无依赖边，可并行 fork），
        //    记录已启动的实例与结果（Requires/Requisite 失败传播用；target 恒为成功）。
        let mut services: Vec<ServiceInstance> = Vec::new();
        let mut started_ok: HashMap<String, bool> = HashMap::new();
        let depths = compute_depths(&order, &units);
        let max_depth = depths.values().max().copied().unwrap_or(0);
        for depth in 0..=max_depth {
            let layer: Vec<&String> = order.iter().filter(|n| depths[*n] == depth).collect();
            // 失败传播检查（主线程按拓扑顺序）：Requires 失败跳过、Requisite
            // 未激活跳过；Wants 失败不传播（尽力依赖）；After 仅排序
            let mut to_start: Vec<&String> = Vec::new();
            for unit_name in &layer {
                let Some(unit) = units.get(*unit_name) else {
                    continue;
                };
                if unit.is_target {
                    log(&format!("rbox init: reached target {}", unit_name));
                    started_ok.insert((*unit_name).clone(), true);
                    continue;
                }
                if let Some(failed_dep) = failed_required_dep(unit, &started_ok) {
                    log_at(
                        LogLevel::Error,
                        &format!(
                            "rbox init: skipping {} because required unit {} failed",
                            unit_name, failed_dep
                        ),
                    );
                    started_ok.insert((*unit_name).clone(), false);
                    continue;
                }
                if let Some(missing_dep) = failed_requisite_dep(unit, &started_ok) {
                    log_at(
                        LogLevel::Error,
                        &format!(
                            "rbox init: skipping {} because requisite unit {} not active",
                            unit_name, missing_dep
                        ),
                    );
                    started_ok.insert((*unit_name).clone(), false);
                    continue;
                }
                to_start.push(unit_name);
            }
            // 同层并发 spawn（spawn 不等待子进程退出，fork 本身是 O(1)；
            // 并发结构为将来同步启动步骤（如 ExecStartPre）预留，且日志更紧凑）
            let mut results: Vec<(String, bool, Option<ServiceInstance>)> =
                std::thread::scope(|s| {
                    let handles: Vec<_> = to_start
                        .iter()
                        .map(|name| {
                            let unit = units.get(*name).unwrap();
                            s.spawn(move || {
                                let (ok, inst) = start_unit(unit);
                                ((*name).clone(), ok, inst)
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .map(|h| h.join().unwrap_or((String::new(), false, None)))
                        .collect()
                });
            // 按拓扑顺序合并结果：services 顺序保持启动顺序（ExecStop 逆序语义不变）
            for unit_name in &layer {
                if let Some((_, ok, inst)) = results
                    .iter_mut()
                    .find(|(n, _, _)| n.as_str() == unit_name.as_str())
                {
                    started_ok.insert((*unit_name).clone(), *ok);
                    if let Some(inst) = inst.take() {
                        services.push(inst);
                    }
                }
            }
        }

        log("rbox init: startup complete");

        // 5. 主循环：回收子进程、响应控制请求，等待关机标志。
        //    服务状态用 Mutex 共享给控制连接线程（见 reap_with_shutdown）。
        let status_listener = create_status_listener();
        let services_shared = Arc::new(Mutex::new(services));
        let units_shared = Arc::new(units);
        reap_with_shutdown(&services_shared, &units_shared, status_listener)
    }
}

/// 早期根切换（initramfs → 内核 root= 指定的持久 rootfs）。
/// 流程：解析 root= → 挂载 proc/dev → 挂载 root 设备到 /newroot →
/// chdir(/newroot) + chroot(".") → exec 新根上的 /init。
/// 成功时进程被替换不会返回；非 early 场景（无 root= / 已是真根）返回 false。
fn early_root_handoff() -> bool {
    use std::ffi::CString;

    // 0. 读 /proc/cmdline 解析 root=。initramfs 早期 /proc 通常未挂载，
    //    先试读，失败才创建目录并挂载 proc（若已由内核/前序挂载则不再重复挂）。
    //    sysfs 切换流程用不到；devtmpfs 仅在确认要切换后才挂（提供 root 设备节点），
    //    避免后续 mount_all_fs 重复挂载报 EBUSY。
    let mut cmdline = std::fs::read_to_string("/proc/cmdline");
    if cmdline.is_err() {
        let _ = std::fs::create_dir_all("/proc");
        let _ = libc_mount("proc", "/proc", "proc");
        cmdline = std::fs::read_to_string("/proc/cmdline");
    }
    let cmdline = cmdline.unwrap_or_default();
    let root_dev = cmdline
        .split_whitespace()
        .find_map(|kv| kv.strip_prefix("root="))
        .map(|v| v.split(',').next().unwrap_or(v).to_string())
        .unwrap_or_default();
    if root_dev.is_empty() {
        return false;
    }

    // 2. 若当前根已经是 ext4（持久 rootfs）则跳过。
    //    不能用 /proc/mounts 判断：chroot 后挂载表仍显示 rootfs，
    //    但进程根实际已是 ext4，会导致二次切换。
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c"/".as_ptr(), &mut st) } == 0
        && st.f_type == libc::EXT4_SUPER_MAGIC as libc::c_long
    {
        return false; // 已是持久 rootfs
    }

    log(&format!(
        "rbox init: switching to persistent root {}",
        root_dev
    ));

    // 3. 挂载 devtmpfs 提供 root 设备节点（内核未自动挂载时）；
    //    已挂载（EBUSY）时节点由内核的 devtmpfs 提供，忽略即可
    let _ = std::fs::create_dir_all("/dev");
    let _ = libc_mount("devtmpfs", "/dev", "devtmpfs");

    // 4. 挂载 root 设备到 /newroot（显式 ext4；自动探测对某些块设备返回 ENXIO）
    let _ = std::fs::create_dir_all("/newroot");
    if let Err(e) = libc_mount(&root_dev, "/newroot", "ext4") {
        log_at(
            LogLevel::Error,
            &format!("rbox init: cannot mount root {}: {}", root_dev, e),
        );
        return false;
    }

    // 5. switch_root：chroot 到新根（pivot_root 与 MS_MOVE 在 initramfs 的
    //    rootfs 根上都受限（EINVAL）；chroot 无需挂载操作，旧 initramfs
    //    挂载树会保留在内存中，约几 MB，可接受）
    unsafe {
        let newroot = CString::new("/newroot").unwrap();
        let root = CString::new("/").unwrap();
        let dot = CString::new(".").unwrap();
        // 先进入新根挂载点，再 chroot(".") 使当前目录成为新根
        if libc::chdir(newroot.as_ptr()) != 0 {
            log_at(
                LogLevel::Error,
                &format!(
                    "rbox init: chdir /newroot failed: {}",
                    std::io::Error::last_os_error()
                ),
            );
            return false;
        }
        if libc::chroot(dot.as_ptr()) != 0 {
            log_at(
                LogLevel::Error,
                &format!(
                    "rbox init: chroot failed: {}",
                    std::io::Error::last_os_error()
                ),
            );
            return false;
        }
        libc::chdir(root.as_ptr());
    }

    // 6. exec 新根的 init（成功则进程被替换）
    let err = std::process::Command::new("/init").exec();
    log_at(
        LogLevel::Error,
        &format!("rbox init: cannot exec /init: {}", err),
    );
    false
}

/// 简单 mount 封装（不解析选项）。
fn libc_mount(src: &str, tgt: &str, fstype: &str) -> std::io::Result<()> {
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
            0,
            std::ptr::null::<std::ffi::c_void>(),
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// 检查单元的 Requires 依赖中是否有启动失败的，返回第一个失败依赖名。
/// After 仅排序，不参与失败传播。
fn failed_required_dep<'a>(
    unit: &'a Unit,
    started_ok: &HashMap<String, bool>,
) -> Option<&'a String> {
    unit.unit
        .requires
        .iter()
        .find(|dep| matches!(started_ok.get(*dep), Some(false)))
}

/// 计算每个单元按拓扑顺序的启动深度（最长依赖链长度；无依赖为 0）。
/// 同深度的单元之间无依赖边，可并行启动。依赖计 Requires/After/Wants（参与
/// 排序）以及 Requisite（不参与拓扑激活，但计入深度：保证前置检查发生时依赖
/// 已在本层之前尝试启动，未激活时检查自然失败）。
/// 同层单元在 order 中的先后不定（WantedBy 反向遍历依赖 HashMap 顺序），
/// 因此深度需要多遍迭代直到收敛，不能依赖单遍顺序。
fn compute_depths(order: &[String], units: &HashMap<String, Unit>) -> HashMap<String, usize> {
    let mut depths: HashMap<String, usize> = HashMap::new();
    loop {
        let mut changed = false;
        for name in order {
            let Some(unit) = units.get(name) else {
                continue;
            };
            let deps = unit
                .unit
                .requires
                .iter()
                .chain(unit.unit.after.iter())
                .chain(unit.unit.wants.iter())
                .chain(unit.unit.requisite.iter());
            let depth = deps
                .filter_map(|d| depths.get(d))
                .max()
                .map(|m| m + 1)
                .unwrap_or(0);
            if depths.get(name) != Some(&depth) {
                depths.insert(name.clone(), depth);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    depths
}

/// 启动一个服务单元（依赖检查由调用方完成）；返回 (是否成功, 运行实例)。
/// spawn 成功与否决定 Requires 失败传播；forking 服务以父进程 spawn 成功为
/// "成功"（daemon 化结果异步，见主循环）；无 ExecStart 的单元视为成功。
fn start_unit(unit: &Unit) -> (bool, Option<ServiceInstance>) {
    let Some(cmd) = &unit.service.exec_start else {
        return (true, None); // 无 ExecStart 的单元（如占位服务）视为启动成功
    };
    if !unit.service.typ.is_empty() && unit.service.typ != "simple" && unit.service.typ != "forking"
    {
        log_at(
            LogLevel::Warn,
            &format!(
                "rbox init: {} Type={:?} unsupported, treating as simple",
                unit.name, unit.service.typ
            ),
        );
    }
    if !unit.service.restart.is_empty()
        && unit.service.restart != "on-failure"
        && unit.service.restart != "always"
        && unit.service.restart != "no"
    {
        log_at(
            LogLevel::Warn,
            &format!(
                "rbox init: {} Restart={:?} unsupported, ignoring",
                unit.name, unit.service.restart
            ),
        );
    }
    if unit.unit.description.is_empty() {
        log(&format!("rbox init: starting {}: {}", unit.name, cmd));
    } else {
        log(&format!(
            "rbox init: starting {} ({}): {}",
            unit.name, unit.unit.description, cmd
        ));
    }
    let env = parse_environment(&unit.service.environment);
    if unit.service.typ == "forking" {
        match start_forking_service(unit, cmd, &env) {
            Some(inst) => (true, Some(inst)),
            None => (false, None),
        }
    } else {
        match start_service(unit, cmd, &env) {
            Some(inst) => (true, Some(inst)),
            None => (false, None),
        }
    }
}

/// 检查 Requisite 依赖是否已成功激活：依赖未启动（不在 started_ok）或
/// 启动失败均视为不满足，返回第一个不满足的依赖名。
/// Requisite 不参与拓扑排序，依赖靠 Requires/After/Wants 或手动 start 激活；
/// 未激活时本单元跳过（systemd 语义：Requisite 失败）。
fn failed_requisite_dep<'a>(
    unit: &'a Unit,
    started_ok: &HashMap<String, bool>,
) -> Option<&'a String> {
    unit.unit
        .requisite
        .iter()
        .find(|dep| !matches!(started_ok.get(*dep), Some(true)))
}

/// 单元加载/依赖解析失败时的降级路径：循环拉起一个 emergency shell。
/// 轮询等待 shell 退出并同时响应关机标志（SIGTERM 到来时不再等 shell 退出）。
/// 启动模式：内核 cmdline 的 `single`/`emergency` 单词决定。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BootMode {
    /// 正常启动（默认）
    Normal,
    /// 单用户模式：跳过服务，进 root shell
    Single,
    /// 应急模式：跳过服务，进 emergency shell
    Emergency,
}

/// 从 /proc/cmdline 解析启动模式：包含单词 `emergency` 或 `single` 时生效
/// （精确单词匹配，避免误匹配 root=/dev/single 之类参数）。
fn boot_mode_from_cmdline() -> BootMode {
    let cmdline = std::fs::read_to_string("/proc/cmdline").unwrap_or_default();
    let words: Vec<&str> = cmdline.split_whitespace().collect();
    boot_mode_from_words(&words)
}

/// 从 cmdline 单词列表解析启动模式（纯函数，便于单测）。
fn boot_mode_from_words(words: &[&str]) -> BootMode {
    if words.contains(&"emergency") {
        BootMode::Emergency
    } else if words.contains(&"single") {
        BootMode::Single
    } else {
        BootMode::Normal
    }
}

/// 打开硬件看门狗（打开即启动计数）。失败静默禁用（无设备环境不阻塞启动）。
fn open_watchdog(path: &str) -> Option<i32> {
    use std::ffi::CString;
    let p = CString::new(path).ok()?;
    let fd = unsafe { libc::open(p.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        log_at(
            LogLevel::Warn,
            &format!(
                "rbox init: watchdog unavailable ({}): watchdog disabled",
                path
            ),
        );
        None
    } else {
        log(&format!("rbox init: watchdog enabled on {}", path));
        Some(fd)
    }
}

/// 喂狗：写入任意字节。成功返回 true；失败返回 false（调用方禁用喂狗）。
fn feed_watchdog(fd: i32) -> bool {
    unsafe { libc::write(fd, b"V".as_ptr() as *const libc::c_void, 1) == 1 }
}

/// poll 超时与喂狗截止取 min：空闲时也能定时醒来喂狗。
/// `watchdog_active=false` 时原样返回（无喂狗约束）。
fn watchdog_poll_timeout(
    current_ms: i32,
    last_feed: &std::time::Instant,
    interval: &std::time::Duration,
    watchdog_active: bool,
) -> i32 {
    if !watchdog_active {
        return current_ms;
    }
    let since = last_feed.elapsed();
    let remain = if since >= *interval {
        0
    } else {
        (*interval - since).as_millis().min(i32::MAX as u128) as i32
    };
    if current_ms < 0 {
        remain // 原无限等待：改为按喂狗间隔唤醒
    } else {
        current_ms.min(remain)
    }
}

/// 应急/单用户 shell：跳过单元加载，循环 spawn root shell；
/// shell 退出后重新拉起，期间响应关机标志（terminate shell 后进入关机流程）。
/// `reason` 用于日志区分（no units / emergency / single）。
fn run_emergency_shell(reason: &str) -> ExitCode {
    log_at(
        LogLevel::Error,
        &format!("rbox init: {} mode, emergency shell", reason),
    );
    // 应急/单用户模式也喂狗：避免诊断期间被硬件看门狗复位打断
    let cfg = crate::config::load();
    let watchdog_interval = std::time::Duration::from_secs(cfg.init.watchdog_interval);
    let mut watchdog_fd = if cfg.init.watchdog_interval > 0 {
        open_watchdog(&cfg.init.watchdog_path)
    } else {
        None
    };
    let mut last_feed = std::time::Instant::now();
    loop {
        if let Some(fd) = watchdog_fd
            && last_feed.elapsed() >= watchdog_interval
        {
            if feed_watchdog(fd) {
                last_feed = std::time::Instant::now();
            } else {
                log_at(
                    LogLevel::Warn,
                    "rbox init: watchdog write failed, disabling",
                );
                watchdog_fd = None;
            }
        }
        if shutdown_requested() {
            return do_shutdown(&mut []);
        }
        let mut child = match spawn_fresh_shell() {
            Some(c) => c,
            None => {
                log("rbox init: cannot spawn emergency shell, waiting");
                std::thread::sleep(std::time::Duration::from_secs(1));
                continue;
            }
        };
        // 轮询等待 shell 退出；期间响应关机标志（终止 shell 后进入关机流程）
        loop {
            if shutdown_requested() {
                let _ = kill_all(libc::SIGTERM);
                let _ = child.wait();
                break;
            }
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(_) => break,
            }
        }
    }
}

/// 安装 SIGTERM/SIGINT/SIGCHLD 信号处理器（sigaction + SA_RESTART）。
/// SIGCHLD 用于唤醒主循环收割子进程；SA_NOCLDSTOP 忽略子进程停止事件。
/// SIGHUP/SIGPIPE/SIGQUIT 显式忽略：PID 1 不能被这些信号终止
/// （tty 断开/写断管道/终端退格符都会触发，一旦命中即 kernel panic）。
fn install_signal_handlers() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        // 未设 SA_SIGINFO：内核按 sa_handler 形式调用单参数处理器。
        // sa_sigaction 与 sa_handler 为 union，这里直接以函数指针赋值。
        sa.sa_sigaction = signal_handler as extern "C" fn(i32) as usize;
        sa.sa_flags = libc::SA_RESTART | libc::SA_NOCLDSTOP;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGCHLD, &sa, std::ptr::null_mut());
        // 以下三个信号保持运行，绝不终止 PID 1：
        // - SIGHUP：控制终端断开（串口拔出/会话首进程挂断）
        // - SIGPIPE：写已关闭的管道（日志/控制连接等）
        // - SIGQUIT：终端 \ 不应能 core dump PID 1
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        libc::signal(libc::SIGQUIT, libc::SIG_IGN);
    }
}

/// 创建 self-pipe（两端 nonblocking + close-on-exec），返回 (读端, 写端)。
fn create_signal_pipe() -> (i32, i32) {
    let mut fds = [0i32; 2];
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return (-1, -1);
        }
        for fd in fds {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
            let fdflags = libc::fcntl(fd, libc::F_GETFD);
            if fdflags >= 0 {
                libc::fcntl(fd, libc::F_SETFD, fdflags | libc::FD_CLOEXEC);
            }
        }
    }
    (fds[0], fds[1])
}

/// 清空 self-pipe 读端（多次信号合并为一次，读空避免积压）。
fn drain_signal_pipe(fd: i32) {
    let mut buf = [0u8; 64];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break;
        }
    }
}

/// 计算 poll 超时（毫秒）：最近的 restart 退避或 daemon 化超时；无则 -1（无限等待）。
fn compute_next_timeout(services: &[ServiceInstance]) -> i32 {
    let now = std::time::Instant::now();
    let mut earliest: Option<std::time::Duration> = None;
    for svc in services {
        if let Some(at) = svc.next_restart_at {
            let d = at.saturating_duration_since(now);
            earliest = Some(match earliest {
                Some(e) => e.min(d),
                None => d,
            });
        }
        if svc.waiting_daemonize
            && let Some(deadline) = svc.daemonize_deadline
        {
            let d = deadline.saturating_duration_since(now);
            earliest = Some(match earliest {
                Some(e) => e.min(d),
                None => d,
            });
        }
    }
    match earliest {
        Some(d) => d.as_millis().min(i32::MAX as u128) as i32,
        None => -1,
    }
}

/// 主循环：回收/重启服务、响应 rservice/rbox status 控制请求，检测关机标志。
/// 所有服务统一由 Restart 策略管理（console/getty 用 Restart=always）。
/// 服务状态用 `Mutex` 保护，控制连接在独立线程处理，主循环不被
/// ExecStop（最坏数秒）等长操作阻塞。
fn reap_with_shutdown(
    services_shared: &Arc<Mutex<Vec<ServiceInstance>>>,
    units: &Arc<HashMap<String, Unit>>,
    status_listener: Option<UnixListener>,
) -> ExitCode {
    // 创建 self-pipe：信号处理器写 1 字节唤醒主循环 poll
    let (signal_pipe_read, signal_pipe_write) = create_signal_pipe();
    SIGNAL_PIPE_WRITE.store(signal_pipe_write, Ordering::SeqCst);

    // 硬件看门狗：主循环存活期间周期喂狗（poll 定时唤醒），
    // 主循环挂死（死锁/异常）即停止喂狗 -> 硬件超时复位整机
    let cfg = crate::config::load();
    let watchdog_interval = std::time::Duration::from_secs(cfg.init.watchdog_interval);
    let mut watchdog_fd = if cfg.init.watchdog_interval > 0 {
        open_watchdog(&cfg.init.watchdog_path)
    } else {
        None
    };
    let mut last_feed = std::time::Instant::now();

    loop {
        // 1. 回收已退出的服务进程 + forking daemon 化等待；
        //    Restart=on-failure/always 时安排重启（退避 + 上限）。
        //    用 try_lock：控制线程正在执行 stop/restart 时短暂等待，
        //    不阻塞也不忙抢（控制线程通常毫秒级完成，最坏 ExecStop 数秒）。
        let mut services_guard = match services_shared.try_lock() {
            Ok(g) => g,
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(50));
                continue;
            }
        };
        let services = &mut *services_guard;
        for svc in services.iter_mut() {
            // 1a. forking 服务等待父进程 daemon 化（异步状态机，不阻塞主循环）
            if svc.waiting_daemonize {
                let result = svc.child.as_mut().map(|child| child.try_wait());
                match result {
                    // 父进程退出 = daemon 化完成：读 PIDFile 跟踪 daemon
                    Some(Ok(Some(_))) => finish_daemonize(svc),
                    Some(Ok(None)) => {
                        // 超时未 daemon 化：kill 并标记失败（触发 Restart 调度）
                        if svc
                            .daemonize_deadline
                            .is_some_and(|d| std::time::Instant::now() >= d)
                        {
                            if let Some(child) = svc.child.as_mut() {
                                let _ = child.kill();
                                let _ = child.wait();
                            }
                            log_at(
                                LogLevel::Warn,
                                &format!(
                                    "rbox init: {} did not daemonize within {}s, killing",
                                    svc.name, svc.timeout_start_sec
                                ),
                            );
                            svc.child = None;
                            svc.waiting_daemonize = false;
                            svc.daemonize_deadline = None;
                            schedule_restart(svc, true);
                        }
                    }
                    // 竞态：状态已被孤儿收割取走，视为 daemon 化完成
                    Some(Err(_)) => finish_daemonize(svc),
                    // child 已被 reap_orphans 处理（finish_daemonize 已调用）
                    None => {}
                }
                continue;
            }
            if let Some(child) = svc.child.as_mut() {
                let (exited, failed) = match child.try_wait() {
                    Ok(Some(status)) => {
                        log_at(
                            LogLevel::Warn,
                            &format!(
                                "rbox init: service {} exited (code {:?})",
                                svc.name,
                                status.code()
                            ),
                        );
                        (true, !status.success())
                    }
                    // 竞态：状态已被孤儿收割取走，视为退出但不触发重启
                    Err(_) => (true, false),
                    Ok(None) => (false, false),
                };
                if exited {
                    svc.child = None;
                    schedule_restart(svc, failed);
                }
            }
            // 1b. 到达 RestartSec 退避时间点则重新拉起
            if let Some(at) = svc.next_restart_at
                && std::time::Instant::now() >= at
                && !shutdown_requested()
                && !svc.stopped
            {
                svc.next_restart_at = None;
                log_at(
                    LogLevel::Info,
                    &format!(
                        "rbox init: restarting {} (attempt {})",
                        svc.name,
                        svc.fail_count + 1
                    ),
                );
                respawn_service(svc);
            }
        }

        // 2. 收割收养的孤儿进程（waitpid -1），防止僵尸累积
        reap_orphans(services);

        // 3. 关机/重启标志
        if shutdown_requested() {
            return do_shutdown(services);
        }

        // 3.5 喂狗：主循环存活证明（poll 唤醒即喂）；设备失效则禁用
        if let Some(fd) = watchdog_fd
            && last_feed.elapsed() >= watchdog_interval
        {
            if feed_watchdog(fd) {
                last_feed = std::time::Instant::now();
            } else {
                log_at(
                    LogLevel::Warn,
                    "rbox init: watchdog write failed, disabling",
                );
                watchdog_fd = None;
            }
        }

        // 4. 事件等待：poll 监听 self-pipe 与 status socket。
        //    超时为最近的 restart 退避 / daemon 化超时 / 喂狗截止
        //    （无定时则按喂狗间隔唤醒，保证空闲时也能定时喂狗）。
        let timeout = watchdog_poll_timeout(
            compute_next_timeout(services),
            &last_feed,
            &watchdog_interval,
            watchdog_fd.is_some(),
        );
        drop(services_guard); // poll 期间释放锁，控制线程可获锁执行请求
        let status_fd = status_listener.as_ref().map(|l| l.as_raw_fd());
        let mut fds = [
            libc::pollfd {
                fd: signal_pipe_read,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: status_fd.unwrap_or(-1),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let nfds = if status_fd.is_some() { 2 } else { 1 };
        let n = unsafe { libc::poll(fds.as_mut_ptr(), nfds as libc::nfds_t, timeout) };
        if n > 0 {
            if fds[0].revents & libc::POLLIN != 0 {
                drain_signal_pipe(signal_pipe_read);
            }
            // 响应控制请求（rbox status / rservice）：独立线程处理，不阻塞主循环
            if fds[1].revents & libc::POLLIN != 0
                && let Some(listener) = &status_listener
                && let Ok((stream, _)) = listener.accept()
            {
                let svc = services_shared.clone();
                let u = units.clone();
                std::thread::spawn(move || handle_control_connection(stream, svc, u));
            }
        } else if n < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
        {
            // 非 EINTR 的 poll 错误：短暂休眠避免忙循环
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
}

/// 收割收养的孤儿进程（waitpid -1, WNOHANG），防止僵尸累积。
/// 已知服务的子进程由 try_wait 先行处理；
/// 若恰在 try_wait 之后退出被这里收割（竞态），同步其状态。
fn reap_orphans(services: &mut [ServiceInstance]) {
    loop {
        let mut status: libc::c_int = 0;
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if pid <= 0 {
            break; // 0 = 无已退出子进程，-1 = 无子进程
        }
        let pid = pid as u32;
        for svc in services.iter_mut() {
            // forking daemon 退出（被收养的 daemon 由这里匹配并触发重启调度）
            if svc.tracked_pid == Some(pid) {
                log_at(
                    LogLevel::Warn,
                    &format!("rbox init: service {} (daemon) exited", svc.name),
                );
                svc.tracked_pid = None;
                let failed = !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0;
                schedule_restart(svc, failed);
                break;
            }
            if let Some(child) = svc.child.as_mut()
                && child.id() == pid
            {
                if svc.waiting_daemonize {
                    // forking 父进程被孤儿收割收割：同样完成 daemon 化
                    finish_daemonize(svc);
                } else {
                    log(&format!("rbox init: service {} reaped", svc.name));
                    svc.child = None;
                }
                break;
            }
        }
    }
}

/// 关机总超时（秒）：逐服务 stop + 残留进程回收共用此 deadline，
/// 到点后直接 SIGKILL 全部残留进程，避免被忽略 SIGTERM 的进程拖住。
const SHUTDOWN_TIMEOUT_SECS: u64 = 10;

/// 执行有序关机：逆序停止服务，杀残留进程，再 power off。
fn do_shutdown(services: &mut [ServiceInstance]) -> ExitCode {
    log("rbox init: shutting down");
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(SHUTDOWN_TIMEOUT_SECS);
    for svc in services.iter_mut().rev() {
        if std::time::Instant::now() >= deadline {
            log_at(
                LogLevel::Warn,
                "rbox init: shutdown deadline reached, skipping remaining services",
            );
            break;
        }
        stop_service_instance(svc);
    }
    log("rbox init: sending SIGTERM to all processes");
    let _ = kill_all(libc::SIGTERM);
    // 等待所有子进程退出（受总 deadline 约束）；到点升级 SIGKILL，避免忽略
    // SIGTERM 的进程无限拖延关机。
    loop {
        if std::time::Instant::now() >= deadline {
            log("rbox init: sending SIGKILL to all processes");
            let _ = kill_all(libc::SIGKILL);
            // 给 SIGKILL 一个极短收割窗口（最多 1 秒）
            let kill_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            while std::time::Instant::now() < kill_deadline {
                let mut status: libc::c_int = 0;
                let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
                if pid < 0 {
                    break; // ECHILD：无子进程
                }
                if pid == 0 {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            break;
        }
        let mut status: libc::c_int = 0;
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if pid < 0 {
            break; // ECHILD：无子进程
        }
        if pid == 0 {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // pid > 0：已收割一个，立即继续收割其余
    }
    sync_fs();
    // 有序关机：先把根文件系统 remount 只读，再触发 reboot 系统调用
    // （initramfs 根已是 tmpfs/rootfs 且不可 remount ro 时忽略失败，仅尽力而为）
    let root = std::ffi::CString::new("/").unwrap();
    let opts = std::ffi::CString::new("remount,ro").unwrap();
    let _ = unsafe {
        libc::mount(
            std::ptr::null(),
            root.as_ptr(),
            std::ptr::null(),
            libc::MS_REMOUNT,
            opts.as_ptr() as *const libc::c_void,
        )
    };
    let is_reboot = REBOOT_REQUESTED.load(Ordering::SeqCst);
    if is_reboot {
        log("rbox init: rebooting");
    } else {
        log("rbox init: power off");
    }
    let action = if is_reboot {
        libc::RB_AUTOBOOT
    } else {
        libc::RB_POWER_OFF
    };
    if let Err(e) = reboot_syscall(action) {
        log_at(
            LogLevel::Error,
            &format!("rbox init: reboot syscall failed: {}", e),
        );
        // 重启失败时回退到关机；仍失败则挂起等待人工干预
        if is_reboot {
            let _ = reboot_syscall(libc::RB_POWER_OFF);
        }
    }
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::applets::core::init::services::test_svc;

    /// 从 TOML 构造一个带 Requires 列表的测试单元。
    fn unit_with_requires(requires: &[&str]) -> Unit {
        let quoted = requires
            .iter()
            .map(|r| format!("\"{}\"", r))
            .collect::<Vec<_>>()
            .join(", ");
        let mut u: Unit =
            toml::from_str(&format!("[Unit]\nName = \"t\"\nRequires = [{}]\n", quoted)).unwrap();
        u.name = "t".to_string();
        u
    }

    #[test]
    fn failed_required_dep_detects_failure() {
        let u = unit_with_requires(&["a.service", "b.service"]);
        let mut ok = HashMap::new();
        ok.insert("a.service".to_string(), true);
        ok.insert("b.service".to_string(), false);
        assert_eq!(failed_required_dep(&u, &ok), Some(&"b.service".to_string()));
    }

    #[test]
    fn boot_mode_from_words_matches_exact_words() {
        assert_eq!(boot_mode_from_words(&[]), BootMode::Normal);
        assert_eq!(
            boot_mode_from_words(&["console=ttyAMA0", "rdinit=/init"]),
            BootMode::Normal
        );
        assert_eq!(
            boot_mode_from_words(&["root=/dev/vda", "single"]),
            BootMode::Single
        );
        assert_eq!(boot_mode_from_words(&["emergency"]), BootMode::Emergency);
        // emergency 优先于 single（systemd 语义：emergency 更深）
        assert_eq!(
            boot_mode_from_words(&["single", "emergency"]),
            BootMode::Emergency
        );
        // 非精确单词不误匹配（root=/dev/single 之类）
        assert_eq!(
            boot_mode_from_words(&["root=/dev/single"]),
            BootMode::Normal
        );
        assert_eq!(
            boot_mode_from_words(&["console=ttyAMA0,emergency"]),
            BootMode::Normal
        );
    }

    #[test]
    fn failed_required_dep_all_ok() {
        let u = unit_with_requires(&["a.service"]);
        let mut ok = HashMap::new();
        ok.insert("a.service".to_string(), true);
        assert_eq!(failed_required_dep(&u, &ok), None);
    }

    #[test]
    fn failed_required_dep_missing_dep_is_not_failure() {
        // 缺失依赖（不在 started_ok）不算失败——已在拓扑阶段告警
        let u = unit_with_requires(&["ghost.service"]);
        let ok = HashMap::new();
        assert_eq!(failed_required_dep(&u, &ok), None);
    }

    #[test]
    fn failed_requisite_dep_not_started_is_failure() {
        // Requisite 依赖未激活（不在 started_ok）-> 本单元跳过
        let mut u = unit_with_requires(&[]);
        u.unit.requisite = vec!["b.service".to_string()];
        let ok = HashMap::new();
        assert_eq!(
            failed_requisite_dep(&u, &ok),
            Some(&"b.service".to_string())
        );
    }

    #[test]
    fn failed_requisite_dep_failed_is_failure() {
        let mut u = unit_with_requires(&[]);
        u.unit.requisite = vec!["b.service".to_string()];
        let mut ok = HashMap::new();
        ok.insert("b.service".to_string(), false);
        assert_eq!(
            failed_requisite_dep(&u, &ok),
            Some(&"b.service".to_string())
        );
    }

    #[test]
    fn failed_requisite_dep_started_ok() {
        let mut u = unit_with_requires(&[]);
        u.unit.requisite = vec!["b.service".to_string()];
        let mut ok = HashMap::new();
        ok.insert("b.service".to_string(), true);
        assert_eq!(failed_requisite_dep(&u, &ok), None);
    }

    #[test]
    fn compute_depths_chains() {
        // a(0) -> b(1) -> c(2)；wants 计入深度，requisite 也计入（保证检查时依赖已尝试启动）
        let mut units = HashMap::new();
        units.insert("a.service".into(), unit_with_requires(&[]));
        let mut b = unit_with_requires(&["a.service"]);
        b.unit.wants = vec!["x.service".to_string()];
        units.insert("b.service".into(), b);
        let mut c = unit_with_requires(&["b.service"]);
        c.unit.requisite = vec!["z.service".to_string()];
        units.insert("c.service".into(), c);
        // z.service 被拓扑激活（在 order 中）：requisite 计入深度 -> c 深度 3
        units.insert("z.service".into(), unit_with_requires(&[]));
        let order = vec![
            "a.service".to_string(),
            "b.service".to_string(),
            "c.service".to_string(),
            "z.service".to_string(),
        ];
        let depths = compute_depths(&order, &units);
        assert_eq!(depths["a.service"], 0);
        assert_eq!(depths["b.service"], 1); // wants 计入
        assert_eq!(depths["z.service"], 0);
        assert_eq!(depths["c.service"], 2); // max(requires b=1, requisite z=0) + 1
    }

    #[test]
    fn compute_depths_siblings_share_depth() {
        // 同层（无依赖边）深度相同，可并行
        let mut units = HashMap::new();
        units.insert("a.service".into(), unit_with_requires(&[]));
        units.insert("b.service".into(), unit_with_requires(&[]));
        units.insert("c.service".into(), unit_with_requires(&["a.service"]));
        let order = vec![
            "a.service".to_string(),
            "b.service".to_string(),
            "c.service".to_string(),
        ];
        let depths = compute_depths(&order, &units);
        assert_eq!(depths["a.service"], 0);
        assert_eq!(depths["b.service"], 0); // 与 a 同层
        assert_eq!(depths["c.service"], 1);
    }

    #[test]
    fn compute_depths_converges_when_dep_later_in_order() {
        // 回归：requisite 依赖在 order 中排在后面（同层顺序不定），
        // 深度必须多遍迭代收敛而非依赖单遍顺序
        let mut units = HashMap::new();
        units.insert("dep.service".into(), unit_with_requires(&[]));
        let mut u = unit_with_requires(&[]);
        u.unit.requisite = vec!["dep.service".to_string()];
        units.insert("u.service".into(), u);
        // dep 排在 u 之后（模拟 HashMap 遍历顺序不定）
        let order = vec!["u.service".to_string(), "dep.service".to_string()];
        let depths = compute_depths(&order, &units);
        assert_eq!(depths["dep.service"], 0);
        assert_eq!(depths["u.service"], 1); // 依赖在 order 后面也能正确传播
    }

    #[test]
    #[test]
    fn watchdog_poll_timeout_infinite_becomes_interval() {
        // 原无限等待（-1）：有喂狗约束时改为按喂狗间隔唤醒
        let last = std::time::Instant::now();
        let iv = std::time::Duration::from_secs(10);
        let t = watchdog_poll_timeout(-1, &last, &iv, true);
        assert!((9000..=10000).contains(&t), "t={t}");
    }

    #[test]
    fn watchdog_poll_timeout_min_with_existing() {
        // 已有更短超时（如 restart 退避 1s）保持；更长超时被喂狗截止压缩
        let last = std::time::Instant::now();
        let iv = std::time::Duration::from_secs(10);
        assert_eq!(watchdog_poll_timeout(500, &last, &iv, true), 500);
        let t = watchdog_poll_timeout(30_000, &last, &iv, true);
        assert!((9000..=10000).contains(&t), "t={t}");
    }

    #[test]
    fn watchdog_poll_timeout_inactive_passthrough() {
        // 未启用喂狗（无设备）：原超时原样返回，不影响事件驱动
        let last = std::time::Instant::now();
        let iv = std::time::Duration::from_secs(10);
        assert_eq!(watchdog_poll_timeout(-1, &last, &iv, false), -1);
        assert_eq!(watchdog_poll_timeout(200, &last, &iv, false), 200);
    }

    #[test]
    fn watchdog_poll_timeout_due_now() {
        // 已到喂狗时间：立即返回 0（poll 不等待，直接醒来喂狗）
        let last = std::time::Instant::now() - std::time::Duration::from_secs(10);
        let iv = std::time::Duration::from_secs(10);
        assert_eq!(watchdog_poll_timeout(-1, &last, &iv, true), 0);
    }

    #[test]
    fn timeout_infinite_without_timers() {
        let services = vec![test_svc("a.service", false)];
        assert_eq!(compute_next_timeout(&services), -1);
    }

    #[test]
    fn timeout_uses_earliest_restart() {
        let mut svc = test_svc("a.service", true);
        svc.next_restart_at = Some(std::time::Instant::now() + std::time::Duration::from_secs(2));
        let t = compute_next_timeout(&[svc]);
        assert!((1800..=2000).contains(&t), "timeout={t}");
    }

    #[test]
    fn timeout_uses_daemonize_deadline() {
        let mut svc = test_svc("a.service", false);
        svc.waiting_daemonize = true;
        svc.daemonize_deadline =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(500));
        let t = compute_next_timeout(&[svc]);
        assert!((400..=500).contains(&t), "timeout={t}");
    }
}
