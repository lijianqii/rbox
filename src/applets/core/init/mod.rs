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

pub(crate) mod boot;
pub(crate) mod cgroup;
pub(crate) mod mount;
pub(crate) mod notify;
pub(crate) mod randseed;
pub(crate) mod server;
pub(crate) mod services;
pub(crate) mod shutdown;
pub(crate) mod signals;
pub(crate) mod syscall;
pub(crate) mod triggers;
pub(crate) mod units;
pub(crate) mod watchdog;

use crate::applet::Applet;
use crate::applets::core::init::mount::{
    apply_sysctl, mount_all_fs, setup_environment, setup_hostname,
};
use crate::applets::core::init::notify::{NotifySocket, parse_message};
use crate::applets::core::init::server::{create_status_listener, handle_control_connection};
use crate::applets::core::init::services::{
    ServiceInstance, SpawnConfig, exit_is_success, finish_daemonize, oneshot_instance,
    respawn_service, run_command_sync, schedule_restart, spawn_socket_command,
    start_forking_service, start_service, stop_service_instance, unit_environment,
};
use crate::applets::core::init::triggers::{
    PathInstance, SocketInstance, TimerInstance, path_from_unit, socket_from_unit, timer_from_unit,
};
use crate::applets::core::init::units::{Unit, compute_start_order, load_all_units, sort_deps};
use crate::applets::core::{LogLevel, log, log_at};
use std::collections::HashMap;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

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
            signals::install_signal_handlers();
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
        crate::applets::core::init::cgroup::ensure_mounted();
        // 随机种子：尽早恢复（crng 就绪前也能受益）
        crate::applets::core::init::randseed::load();
        // 通知套接字须在服务启动前创建，否则 Type=notify 的 READY 会丢失
        let notify_sock = crate::applets::core::init::notify::create();
        setup_hostname();
        apply_sysctl(&crate::config::load().paths.sysctl_conf);
        log("rbox init: basic filesystems mounted");

        // 1.5 内核 cmdline 的 single/emergency：跳过单元加载，直接进应急/单用户 shell
        if is_pid1 {
            match boot::boot_mode_from_cmdline() {
                boot::BootMode::Emergency => return boot::run_emergency_shell("emergency"),
                boot::BootMode::Single => return boot::run_emergency_shell("single"),
                boot::BootMode::Normal => {}
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
                return boot::run_emergency_shell("no units");
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
                return boot::run_emergency_shell("no units");
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
                    // target 也检查 Requires：依赖失败时标记未达成，触发 rescue
                    if let Some(failed_dep) = failed_required_dep(unit, &started_ok) {
                        log_at(
                            LogLevel::Error,
                            &format!(
                                "rbox init: target {} not reached because required unit {} failed",
                                unit_name, failed_dep
                            ),
                        );
                        started_ok.insert((*unit_name).clone(), false);
                        continue;
                    }
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
            // 启动后短暂等待，检测后续层 Requires 依赖的“秒退”服务
            // （spawn 成功但启动窗口内非零退出），供依赖传播与 rescue 降级使用
            let needed: std::collections::HashSet<String> = order
                .iter()
                .filter(|n| depths[*n] > depth)
                .filter_map(|n| units.get(n))
                .flat_map(|u| u.unit.requires.iter().cloned())
                .collect();
            detect_immediate_failures(&mut services, &mut started_ok, &needed);
        }

        log("rbox init: startup complete");

        // 启动失败降级：default.target 未达成 -> 停止已启动服务并进入 rescue shell
        if started_ok.get(default_target) != Some(&true) {
            log_at(
                LogLevel::Error,
                "rbox init: boot target not reached, entering rescue mode",
            );
            for svc in services.iter_mut().rev() {
                stop_service_instance(svc);
            }
            return boot::run_emergency_shell("rescue");
        }

        // 5. 主循环：回收子进程、响应控制请求，等待关机标志。
        //    服务状态用 Mutex 共享给控制连接线程（见 reap_with_shutdown）。
        let status_listener = create_status_listener();
        let services_shared = Arc::new(Mutex::new(services));
        // 定时器/路径监视实例：仅对可达（started_ok）的单元建立
        let timers: Vec<TimerInstance> = order
            .iter()
            .filter(|n| started_ok.get(*n).copied().unwrap_or(false))
            .filter_map(|n| units.get(n))
            .filter_map(timer_from_unit)
            .collect();
        let paths: Vec<PathInstance> = order
            .iter()
            .filter(|n| started_ok.get(*n).copied().unwrap_or(false))
            .filter_map(|n| units.get(n))
            .filter_map(path_from_unit)
            .collect();
        for t in &timers {
            log(&format!(
                "rbox init: timer {} armed (target {})",
                t.name, t.target
            ));
        }
        for p in &paths {
            log(&format!(
                "rbox init: path {} watching {} (target {})",
                p.name, p.path, p.target
            ));
        }
        let sockets: Vec<SocketInstance> = order
            .iter()
            .filter(|n| started_ok.get(*n).copied().unwrap_or(false))
            .filter_map(|n| units.get(n))
            .filter_map(socket_from_unit)
            .collect();
        for s in &sockets {
            log(&format!(
                "rbox init: socket {} listening (target {})",
                s.name, s.target
            ));
        }
        let units_shared = Arc::new(Mutex::new(units));
        reap_with_shutdown(
            &services_shared,
            &units_shared,
            status_listener,
            timers,
            paths,
            sockets,
            notify_sock,
        )
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
    let root_spec = cmdline
        .split_whitespace()
        .find_map(|kv| kv.strip_prefix("root="))
        .map(|v| v.split(',').next().unwrap_or(v).to_string())
        .unwrap_or_default();
    if root_spec.is_empty() {
        return false;
    }
    // 解析 UUID=/PARTUUID=/LABEL=（依赖 /dev/disk/by-* 符号链接，需 udev/mdev 填充）
    let Some(root_dev) = resolve_root_spec(&root_spec, "/dev/disk") else {
        log_at(
            LogLevel::Error,
            &format!(
                "rbox init: cannot resolve root={} (missing /dev/disk/by-* links)",
                root_spec
            ),
        );
        return false;
    };

    // 2. 若当前根已经是 ext4（持久 rootfs）则跳过。
    //    不能用 /proc/mounts 判断：chroot 后挂载表仍显示 rootfs，
    //    但进程根实际已是 ext4，会导致二次切换。
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c"/".as_ptr(), &mut st) } == 0
        && st.f_type as u64 == libc::EXT4_SUPER_MAGIC as u64
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

/// 解析 root= 规格：`UUID=`/`PARTUUID=`/`LABEL=` 查 `<disk_base>/by-*` 符号链接；
/// 其他规格（/dev/vda 等）原样返回；无法解析返回 None。
pub(crate) fn resolve_root_spec(spec: &str, disk_base: &str) -> Option<String> {
    let (subdir, key) = if let Some(k) = spec.strip_prefix("UUID=") {
        ("by-uuid", k)
    } else if let Some(k) = spec.strip_prefix("PARTUUID=") {
        ("by-partuuid", k)
    } else if let Some(k) = spec.strip_prefix("LABEL=") {
        ("by-label", k)
    } else {
        return Some(spec.to_string());
    };
    let path = format!("{}/{}/{}", disk_base, subdir, key);
    if std::path::Path::new(&path).exists() {
        Some(path)
    } else {
        None
    }
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
            let mut deps = sort_deps(name, unit, units);
            // Requisite 不参与拓扑激活，但计入深度：保证前置检查时依赖已在本层之前尝试启动
            deps.extend(unit.unit.requisite.iter().cloned());
            let depth = deps
                .iter()
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

/// 条件检查（ConditionPathExists/ConditionDirectoryNotEmpty，`!` 前缀取反）。
pub(crate) fn conditions_met(unit: &Unit) -> bool {
    let check = |spec: &str, value: bool| {
        let neg = spec.starts_with('!');
        value != neg
    };
    for p in &unit.unit.condition_path_exists {
        if !check(
            p,
            std::path::Path::new(p.trim_start_matches('!').trim_start()).exists(),
        ) {
            return false;
        }
    }
    for d in &unit.unit.condition_dir_not_empty {
        let path = d.trim_start_matches('!').trim_start();
        let nonempty = std::fs::read_dir(path)
            .map(|mut it| it.next().is_some())
            .unwrap_or(false);
        if !check(d, nonempty) {
            return false;
        }
    }
    true
}

/// 运行 ExecStartPost（失败返回 false，调用方需停止已启动的服务）。
fn run_start_post(unit: &Unit, env: &[(String, String)], cfg: &SpawnConfig<'_>) -> bool {
    for post in &unit.service.exec_start_post {
        log(&format!("rbox init: ExecStartPost {}: {}", unit.name, post));
        match run_command_sync(post, env, cfg, unit.service.timeout_start_sec) {
            Some(code) if exit_is_success(unit, Some(code)) => {}
            _ => {
                log_at(
                    LogLevel::Error,
                    &format!("rbox init: {} ExecStartPost failed: {}", unit.name, post),
                );
                return false;
            }
        }
    }
    true
}

/// 启动一个服务单元（依赖检查由调用方完成）；返回 (是否成功, 运行实例)。
/// spawn 成功与否决定 Requires 失败传播；forking 服务以父进程 spawn 成功为
/// "成功"（daemon 化结果异步，见主循环）；Type=oneshot 同步执行并按退出码判定；
/// 条件不满足则跳过（不算失败）；无 ExecStart 的单元视为成功。
fn start_unit(unit: &Unit) -> (bool, Option<ServiceInstance>) {
    // 条件不满足：跳过（systemd 语义：不算失败）
    if !conditions_met(unit) {
        log(&format!(
            "rbox init: {} skipped (condition not met)",
            unit.name
        ));
        return (true, None);
    }
    let Some(cmd) = &unit.service.exec_start else {
        return (true, None); // 无 ExecStart 的单元（如占位服务）视为启动成功
    };
    let typ = unit.service.typ.as_str();
    if !typ.is_empty() && !matches!(typ, "simple" | "forking" | "oneshot" | "notify") {
        log_at(
            LogLevel::Warn,
            &format!(
                "rbox init: {} Type={:?} unsupported, treating as simple",
                unit.name, typ
            ),
        );
    }
    if !unit.service.restart.is_empty()
        && !matches!(
            unit.service.restart.as_str(),
            "no" | "on-failure" | "always" | "on-success" | "on-abnormal" | "on-abort"
        )
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
    let env = unit_environment(unit);
    let cfg = SpawnConfig::from_unit(unit);
    // ExecStartPre：任一失败则启动失败
    for pre in &unit.service.exec_start_pre {
        log(&format!("rbox init: ExecStartPre {}: {}", unit.name, pre));
        match run_command_sync(pre, &env, &cfg, unit.service.timeout_start_sec) {
            Some(code) if exit_is_success(unit, Some(code)) => {}
            _ => {
                log_at(
                    LogLevel::Error,
                    &format!("rbox init: {} ExecStartPre failed: {}", unit.name, pre),
                );
                return (false, None);
            }
        }
    }
    // Type=oneshot：同步执行，按退出码判定成功
    if typ == "oneshot" {
        let code = run_command_sync(cmd, &env, &cfg, unit.service.timeout_start_sec);
        if !exit_is_success(unit, code) {
            log_at(
                LogLevel::Error,
                &format!("rbox init: {} oneshot failed (code {:?})", unit.name, code),
            );
            return (false, None);
        }
        if !run_start_post(unit, &env, &cfg) {
            return (false, None);
        }
        return (true, Some(oneshot_instance(unit, cmd, &env)));
    }
    let started = if typ == "forking" {
        start_forking_service(unit, cmd, &env)
    } else {
        start_service(unit, cmd, &env)
    };
    match started {
        Some(mut inst) => {
            if !run_start_post(unit, &env, &cfg) {
                stop_service_instance(&mut inst);
                return (false, None);
            }
            (true, Some(inst))
        }
        None => (false, None),
    }
}

/// 检测启动阶段“秒退”的服务：spawn 成功但启动窗口内非零退出（Type=simple）。
/// 仅等待 `needed`（后续层 Requires 依赖的单元）中无 Restart 策略的服务；
/// 将失败单元写入 started_ok，供 Requires 依赖传播与 rescue 降级使用。
fn detect_immediate_failures(
    services: &mut [ServiceInstance],
    started_ok: &mut HashMap<String, bool>,
    needed: &std::collections::HashSet<String>,
) {
    if needed.is_empty() {
        return;
    }
    // 最长等待 1s（10ms 轮询）；全部待检服务退出后提前结束。
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        let mut pending = false;
        for svc in services.iter_mut() {
            if !needed.contains(&svc.name)
                || svc.waiting_daemonize
                || svc.stopped
                || svc.restart_always
                || svc.restart_on_failure
            {
                continue;
            }
            let Some(child) = svc.child.as_mut() else {
                continue;
            };
            match child.try_wait() {
                Ok(Some(status)) => {
                    let failed = !status.success();
                    log_at(
                        LogLevel::Warn,
                        &format!(
                            "rbox init: service {} exited during startup (code {:?})",
                            svc.name,
                            status.code()
                        ),
                    );
                    svc.child = None;
                    if failed {
                        started_ok.insert(svc.name.clone(), false);
                    }
                }
                Ok(None) => pending = true,
                Err(_) => {}
            }
        }
        if !pending || std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
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
    units: &Arc<Mutex<HashMap<String, Unit>>>,
    status_listener: Option<UnixListener>,
    mut timers: Vec<TimerInstance>,
    mut paths: Vec<PathInstance>,
    sockets: Vec<SocketInstance>,
    notify_sock: Option<NotifySocket>,
) -> ExitCode {
    // 创建 self-pipe：信号处理器写 1 字节唤醒主循环 poll
    let (signal_pipe_read, signal_pipe_write) = signals::create_signal_pipe();
    signals::set_signal_pipe_write(signal_pipe_write);

    // 硬件看门狗：主循环存活期间周期喂狗（poll 定时唤醒），
    // 主循环挂死（死锁/异常）即停止喂狗 -> 硬件超时复位整机
    let cfg = crate::config::load();
    let watchdog_interval = std::time::Duration::from_secs(cfg.init.watchdog_interval);
    let mut watchdog_fd = if cfg.init.watchdog_interval > 0 {
        watchdog::open_watchdog(&cfg.init.watchdog_path)
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
        // OnFailure=/OnSuccess= 触发的单元（循环结束后统一启动，避免边遍历边 push）
        let mut pending_triggers: Vec<String> = Vec::new();
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
                        (true, !exit_is_success(&svc.unit, status.code()))
                    }
                    // 竞态：状态已被孤儿收割取走，视为退出但不触发重启
                    Err(_) => (true, false),
                    Ok(None) => (false, false),
                };
                if exited {
                    svc.child = None;
                    svc.waiting_ready = false;
                    svc.watchdog_deadline = None;
                    if !failed {
                        // 成功退出：RemainAfterExit=yes 保持 active；触发 OnSuccess
                        if svc.unit.service.remain_after_exit {
                            svc.active = true;
                        }
                        pending_triggers.extend(svc.unit.unit.on_success.iter().cloned());
                    } else {
                        pending_triggers.extend(svc.unit.unit.on_failure.iter().cloned());
                    }
                    if !svc.active {
                        schedule_restart(svc, failed);
                    }
                }
            }
            // 1b. 到达 RestartSec 退避时间点则重新拉起
            if let Some(at) = svc.next_restart_at
                && std::time::Instant::now() >= at
                && !signals::shutdown_requested()
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

        // 1c. OnFailure/OnSuccess：best-effort 启动触发单元（已活动则跳过）
        for name in pending_triggers {
            if services
                .iter()
                .any(|s| s.name == name && (s.child.is_some() || s.active))
            {
                continue;
            }
            let u_opt = units.lock().ok().and_then(|m| m.get(&name).cloned());
            if let Some(u) = u_opt {
                let (ok, inst) = start_unit(&u);
                log_at(
                    if ok { LogLevel::Info } else { LogLevel::Warn },
                    &format!(
                        "rbox init: triggered {} -> {}",
                        name,
                        if ok { "ok" } else { "failed" }
                    ),
                );
                if let Some(inst) = inst {
                    services.push(inst);
                }
            } else {
                log_at(
                    LogLevel::Warn,
                    &format!("rbox init: triggered unit {} not found", name),
                );
            }
        }

        // 1d. 定时器与路径监视：到期/命中则启动关联单元
        let now = std::time::Instant::now();
        let mut trigger_targets: Vec<String> = Vec::new();
        for t in timers.iter_mut() {
            if now >= t.next_due {
                log(&format!(
                    "rbox init: timer {} fired -> {}",
                    t.name, t.target
                ));
                trigger_targets.push(t.target.clone());
                match t.repeat_secs {
                    Some(secs) if secs > 0 => {
                        t.next_due = now + std::time::Duration::from_secs(secs);
                    }
                    _ => {
                        // 一次性定时器：标记为不再触发（下次到期设为极远）
                        t.next_due = now + std::time::Duration::from_secs(86400 * 365);
                    }
                }
            }
        }
        for p in paths.iter_mut() {
            if p.check() {
                log(&format!(
                    "rbox init: path {} triggered -> {}",
                    p.name, p.target
                ));
                trigger_targets.push(p.target.clone());
            }
        }
        for name in trigger_targets {
            let u_opt = units.lock().ok().and_then(|m| m.get(&name).cloned());
            if let Some(u) = u_opt {
                if services
                    .iter()
                    .any(|s| s.name == name && (s.child.is_some() || s.active))
                {
                    continue; // 已在运行
                }
                let (ok, inst) = start_unit(&u);
                log_at(
                    if ok { LogLevel::Info } else { LogLevel::Warn },
                    &format!(
                        "rbox init: timer/path triggered {} -> {}",
                        name,
                        if ok { "ok" } else { "failed" }
                    ),
                );
                if let Some(inst) = inst {
                    services.push(inst);
                }
            } else {
                log_at(
                    LogLevel::Warn,
                    &format!("rbox init: triggered unit {} not found", name),
                );
            }
        }

        // 1e. Type=notify 看门狗：超时未收到 WATCHDOG=1 则终止并触发重启
        let now = std::time::Instant::now();
        for svc in services.iter_mut() {
            if let Some(deadline) = svc.watchdog_deadline
                && now >= deadline
            {
                log_at(
                    LogLevel::Warn,
                    &format!("rbox init: {} watchdog timeout, killing", svc.name),
                );
                svc.watchdog_deadline = None;
                if let Some(child) = svc.child.as_ref() {
                    let _ = crate::applets::core::init::syscall::kill_process_group(
                        child.id(),
                        libc::SIGTERM,
                    );
                } else if let Some(pid) = svc.tracked_pid {
                    let _ =
                        crate::applets::core::init::syscall::kill_process_group(pid, libc::SIGTERM);
                }
            }
        }

        // 2. 收割收养的孤儿进程（waitpid -1），防止僵尸累积
        reap_orphans(services);

        // 3. 关机/重启标志
        if signals::shutdown_requested() {
            return shutdown::do_shutdown(services);
        }

        // 3.5 喂狗：主循环存活证明（poll 唤醒即喂）；设备失效则禁用
        if let Some(fd) = watchdog_fd
            && last_feed.elapsed() >= watchdog_interval
        {
            if watchdog::feed_watchdog(fd) {
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
        let mut next_timeout = compute_next_timeout(services);
        for t in &timers {
            let d = t
                .next_due
                .saturating_duration_since(std::time::Instant::now())
                .as_millis()
                .min(i32::MAX as u128) as i32;
            next_timeout = match next_timeout {
                -1 => d,
                v => v.min(d),
            };
        }
        if !paths.is_empty() {
            // 路径监视按 1s 粒度轮询
            next_timeout = match next_timeout {
                -1 => 1000,
                v => v.min(1000),
            };
        }
        let timeout = watchdog::watchdog_poll_timeout(
            next_timeout,
            &last_feed,
            &watchdog_interval,
            watchdog_fd.is_some(),
        );
        drop(services_guard); // poll 期间释放锁，控制线程可获锁执行请求
        let status_fd = status_listener.as_ref().map(|l| l.as_raw_fd());
        let mut fds = vec![
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
        for sock in &sockets {
            fds.push(libc::pollfd {
                fd: sock.fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        let notify_idx = notify_sock.as_ref().map(|n| {
            fds.push(libc::pollfd {
                fd: n.fd(),
                events: libc::POLLIN,
                revents: 0,
            });
            fds.len() - 1
        });
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if n > 0 {
            if fds[0].revents & libc::POLLIN != 0 {
                signals::drain_signal_pipe(signal_pipe_read);
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
            // socket 激活：可读即接受连接/启动服务（重新持锁，poll 前已释放）
            let mut svc_guard = services_shared.lock().unwrap();
            for (idx, sock) in sockets.iter().enumerate() {
                let pi = 2 + idx;
                if fds[pi].revents & libc::POLLIN == 0 {
                    continue;
                }
                let u_opt = units.lock().ok().and_then(|m| m.get(&sock.target).cloned());
                let Some(u) = u_opt else {
                    log_at(
                        LogLevel::Warn,
                        &format!(
                            "rbox init: socket {} target {} not found",
                            sock.name, sock.target
                        ),
                    );
                    continue;
                };
                let env = unit_environment(&u);
                if sock.accept {
                    if let Some(conn) = sock.accept_conn()
                        && let Some(child) = spawn_socket_command(&u, &env, Some(conn), None)
                    {
                        svc_guard.push(crate::applets::core::init::services::socket_instance(
                            &u, child,
                        ));
                    }
                } else if let Some(child) = spawn_socket_command(&u, &env, None, Some(sock.fd())) {
                    svc_guard.push(crate::applets::core::init::services::socket_instance(
                        &u, child,
                    ));
                }
            }
            // sd_notify：READY=1 / WATCHDOG=1 / STATUS=
            if let Some(idx) = notify_idx
                && fds[idx].revents & libc::POLLIN != 0
                && let Some(ns) = notify_sock.as_ref()
            {
                for (pid, msg) in ns.drain() {
                    let (ready, wd, status, ppid) = parse_message(&msg);
                    if let Some(s) = status {
                        log_at(
                            LogLevel::Debug,
                            &format!("rbox init: notify pid {} status {}", pid, s),
                        );
                    }
                    for svc in svc_guard.iter_mut() {
                        // 发送者可能是服务的子进程（如 sh -c 中的 rbox --sd-notify），
                        // 沿 /proc/<pid>/stat 的 ppid 链向上匹配服务主进程
                        let own = svc.child.as_ref().map(|c| c.id() as i32) == Some(pid)
                            || svc.tracked_pid == Some(pid as u32)
                            || (ppid.is_some()
                                && svc.child.as_ref().map(|c| c.id() as i32) == ppid)
                            || svc
                                .child
                                .as_ref()
                                .is_some_and(|c| is_descendant(pid, c.id() as i32));
                        if !own {
                            continue;
                        }
                        if ready && svc.waiting_ready {
                            svc.waiting_ready = false;
                            log(&format!("rbox init: {} ready (sd_notify)", svc.name));
                            if let Some(secs) = svc.watchdog_secs {
                                svc.watchdog_deadline = Some(
                                    std::time::Instant::now()
                                        + std::time::Duration::from_secs(secs),
                                );
                            }
                        }
                        if wd && let Some(secs) = svc.watchdog_secs {
                            svc.watchdog_deadline = Some(
                                std::time::Instant::now() + std::time::Duration::from_secs(secs),
                            );
                        }
                    }
                }
            }
        } else if n < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
        {
            // 非 EINTR 的 poll 错误：短暂休眠避免忙循环
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
}

/// pid 是否为 ancestor 的后代（沿 /proc/<pid>/stat 的 ppid 链，最多 32 层）。
fn is_descendant(pid: i32, ancestor: i32) -> bool {
    let mut cur = pid;
    for _ in 0..32 {
        if cur == ancestor {
            return true;
        }
        if cur <= 1 {
            return false;
        }
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", cur)) else {
            return false;
        };
        // comm 可能含空格/括号：取最后一个 ')' 之后字段（state ppid ...）
        let Some((_, rest)) = stat.rsplit_once(')') else {
            return false;
        };
        let ppid = rest
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(0);
        if ppid <= 0 {
            return false;
        }
        cur = ppid;
    }
    false
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
    fn resolve_root_spec_plain_device() {
        assert_eq!(
            resolve_root_spec("/dev/vda", "/dev/disk"),
            Some("/dev/vda".to_string())
        );
    }

    #[test]
    fn resolve_root_spec_uuid_lookup() {
        let base = format!("/tmp/rbox_disk_{}", std::process::id());
        std::fs::create_dir_all(format!("{}/by-uuid", base)).unwrap();
        std::fs::write(format!("{}/by-uuid/abc-123", base), "").unwrap();
        assert_eq!(
            resolve_root_spec("UUID=abc-123", &base),
            Some(format!("{}/by-uuid/abc-123", base))
        );
        assert_eq!(resolve_root_spec("UUID=missing", &base), None);
        assert_eq!(resolve_root_spec("LABEL=nope", &base), None);
        let _ = std::fs::remove_dir_all(&base);
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
