//! 服务运行时：spawn、生命周期、重启调度、停止。

use crate::applets::core::init::shutdown_requested;
use crate::applets::core::init::syscall::{kill_process, kill_process_group};
use crate::applets::core::init::units::{Unit, parse_cmdline};
use crate::applets::core::{LogLevel, log, log_at};
use std::os::unix::process::CommandExt;
use std::process::{Child, Stdio};

/// 已启动服务的运行时记录。
/// 持有 `Child` 句柄以便主循环 try_wait 回收，避免僵尸进程。
pub(crate) struct ServiceInstance {
    pub(crate) name: String,
    pub(crate) child: Option<Child>,
    /// Type=forking 的 daemon pid（被 init 收养，退出时由孤儿收割匹配）
    pub(crate) tracked_pid: Option<u32>,
    pub(crate) exec_stop: Option<String>,
    pub(crate) exec_reload: Option<String>,
    /// ExecStart 原文，Restart=on-failure 时用于重新拉起
    pub(crate) exec_start: String,
    /// 服务环境变量（已解析的 VAR=value 对）
    pub(crate) env: Vec<(String, String)>,
    /// 重启时保留的 spawn 配置
    pub(crate) logfile: Option<String>,
    pub(crate) user: Option<String>,
    pub(crate) group: Option<String>,
    /// Type=forking 相关（重启时重新走 daemon 化流程）
    pub(crate) is_forking: bool,
    pub(crate) pidfile: Option<String>,
    pub(crate) timeout_start_sec: u64,
    /// 进程组 id（spawn 父进程时的 pid）；forking 服务用于进程组清理
    pub(crate) pgid: Option<u32>,
    /// forking 服务：已 spawn 父进程、等待其 daemon 化（异步状态机）
    pub(crate) waiting_daemonize: bool,
    /// forking 服务 daemon 化超时时间点
    pub(crate) daemonize_deadline: Option<std::time::Instant>,
    /// Restart=on-failure：非零退出时自动重启
    pub(crate) restart_on_failure: bool,
    /// Restart=always：无论退出码如何都自动重启（console/getty 等服务用）
    pub(crate) restart_always: bool,
    /// 自动重启间隔与连续失败上限
    pub(crate) restart_sec: u64,
    pub(crate) start_limit_burst: u32,
    /// 失败计数时间窗（秒）：距首次失败超过该时长则计数重置
    pub(crate) start_limit_interval_sec: u64,
    /// 窗口内首次失败时间点（用于时间窗重置）
    pub(crate) first_failure_at: Option<std::time::Instant>,
    /// 连续失败次数（成功退出或手动 start 时清零）
    pub(crate) fail_count: u32,
    /// 待重启时间点（RestartSec 退避调度）
    pub(crate) next_restart_at: Option<std::time::Instant>,
    /// stop 请求后标记：禁止自动重启（Restart=on-failure 也不重启）
    pub(crate) stopped: bool,
}

impl ServiceInstance {
    /// status 输出的一行状态。
    pub(crate) fn status_line(&self) -> String {
        let restart = if self.restart_always {
            " restart=always"
        } else if self.restart_on_failure {
            " restart=on-failure"
        } else {
            ""
        };
        if let Some(pid) = self.tracked_pid {
            return format!("{} running pid={}{}\n", self.name, pid, restart);
        }
        match &self.child {
            Some(c) => format!("{} running pid={}{}\n", self.name, c.id(), restart),
            None => {
                let state = if self.stopped { "stopped" } else { "exited" };
                format!("{} {}{}\n", self.name, state, restart)
            }
        }
    }
}

/// spawn 附加配置（LogFile/User/Group）。
pub(crate) struct SpawnConfig<'a> {
    /// stdout/stderr 重定向文件
    pub(crate) logfile: Option<&'a str>,
    /// 降权用户/组名
    pub(crate) user: Option<&'a str>,
    pub(crate) group: Option<&'a str>,
}

impl SpawnConfig<'_> {
    pub(crate) fn from_unit(unit: &Unit) -> SpawnConfig<'_> {
        SpawnConfig {
            logfile: unit.service.logfile.as_deref(),
            user: unit.service.user.as_deref(),
            group: unit.service.group.as_deref(),
        }
    }
}

/// 解析 ExecStart 命令并 spawn 子进程，返回子进程句柄。
/// ExecStart 以 `rbox` 或 `/bin/rbox` 开头时统一走 /bin/rbox。
/// 服务放入独立进程组（process_group），关机时可按组清理其后代进程。
pub(crate) fn spawn_unit_command(
    name: &str,
    cmd: &str,
    env: &[(String, String)],
    cfg: &SpawnConfig<'_>,
) -> Option<Child> {
    let argv = parse_cmdline(cmd);
    if argv.is_empty() {
        return None;
    }
    let (program, args) = if (argv[0] == "rbox" || argv[0] == "/bin/rbox") && argv.len() >= 2 {
        ("/bin/rbox", &argv[1..])
    } else {
        (argv[0].as_str(), &argv[1..])
    };
    let mut command = std::process::Command::new(program);
    command.args(args);
    command.envs(env.iter().cloned());
    command.process_group(0);
    // 输出重定向到日志文件（追加），超过阈值则轮转，否则继承 console
    if let Some(path) = cfg.logfile {
        rotate_log_if_needed(path);
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            Ok(f) => match f.try_clone() {
                Ok(f2) => {
                    let _ = command.stdout(Stdio::from(f));
                    let _ = command.stderr(Stdio::from(f2));
                }
                Err(_) => {
                    let _ = command.stdout(Stdio::from(f));
                }
            },
            Err(e) => {
                log(&format!(
                    "rbox init: cannot open log file {} for {}: {}",
                    path, name, e
                ));
                return None;
            }
        }
    }
    // 降权：User=/Group=（getpwnam/getgrnam 解析，失败则拒绝启动）
    if let Some(user) = cfg.user {
        match lookup_uid(user) {
            Some(uid) => {
                command.uid(uid);
            }
            None => {
                log(&format!("rbox init: unknown user '{}' for {}", user, name));
                return None;
            }
        }
    }
    if let Some(group) = cfg.group {
        match lookup_gid(group) {
            Some(gid) => {
                command.gid(gid);
            }
            None => {
                log(&format!(
                    "rbox init: unknown group '{}' for {}",
                    group, name
                ));
                return None;
            }
        }
    }
    match command.spawn() {
        Ok(child) => {
            log(&format!("rbox init: started {} (pid {})", name, child.id()));
            Some(child)
        }
        Err(e) => {
            log(&format!("rbox init: failed to start {}: {}", name, e));
            None
        }
    }
}

/// 通过 /etc/passwd 解析用户名 -> uid。
fn lookup_uid(name: &str) -> Option<u32> {
    let Ok(c) = std::ffi::CString::new(name) else {
        return None;
    };
    unsafe {
        let pwd = libc::getpwnam(c.as_ptr());
        if pwd.is_null() {
            None
        } else {
            Some((*pwd).pw_uid)
        }
    }
}

/// 通过 /etc/group 解析组名 -> gid。
fn lookup_gid(name: &str) -> Option<u32> {
    let Ok(c) = std::ffi::CString::new(name) else {
        return None;
    };
    unsafe {
        let grp = libc::getgrnam(c.as_ptr());
        if grp.is_null() {
            None
        } else {
            Some((*grp).gr_gid)
        }
    }
}

/// 解析 [Service] Environment 列表为 (VAR, value) 对；跳过非法项。
pub(crate) fn parse_environment(envs: &[String]) -> Vec<(String, String)> {
    envs.iter()
        .filter_map(|e| {
            let (k, v) = e.split_once('=')?;
            if k.is_empty() {
                return None;
            }
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

/// 启动一个 simple 类型服务，返回运行时实例。
pub(crate) fn start_service(
    unit: &Unit,
    cmd: &str,
    env: &[(String, String)],
) -> Option<ServiceInstance> {
    let cfg = SpawnConfig::from_unit(unit);
    let child = spawn_unit_command(&unit.name, cmd, env, &cfg)?;
    Some(new_service_instance(unit, cmd, env, Some(child), None))
}

/// 启动一个 forking 类型服务：spawn 父进程后立即返回（异步 daemon 化）。
/// 父进程退出（daemon 化完成）由主循环检测，届时读 PIDFile 跟踪 daemon pid
/// （被 init 收养，退出由孤儿收割匹配）。超时由主循环按 TimeoutStartSec 处理。
pub(crate) fn start_forking_service(
    unit: &Unit,
    cmd: &str,
    env: &[(String, String)],
) -> Option<ServiceInstance> {
    let cfg = SpawnConfig::from_unit(unit);
    let child = spawn_unit_command(&unit.name, cmd, env, &cfg)?;
    let mut inst = new_service_instance(unit, cmd, env, Some(child), None);
    inst.pgid = inst.child.as_ref().map(|c| c.id());
    inst.waiting_daemonize = true;
    inst.daemonize_deadline = Some(
        std::time::Instant::now() + std::time::Duration::from_secs(unit.service.timeout_start_sec),
    );
    Some(inst)
}

/// forking 服务父进程退出：完成 daemon 化，读 PIDFile 设置 tracked_pid。
/// 无 PIDFile 时无法跟踪 daemon，告警说明 Restart=on-failure 在 daemon
/// 崩溃时不会触发（需要 cgroup 才能可靠跟踪，当前未实现）。
pub(crate) fn finish_daemonize(svc: &mut ServiceInstance) {
    svc.child = None;
    svc.waiting_daemonize = false;
    svc.daemonize_deadline = None;
    svc.tracked_pid = svc.pidfile.as_deref().and_then(read_pid_file);
    if svc.pidfile.is_some() && svc.tracked_pid.is_none() {
        log_at(
            LogLevel::Warn,
            &format!(
                "rbox init: {} daemonized but PIDFile unreadable, not tracking",
                svc.name
            ),
        );
    } else if svc.pidfile.is_none() {
        log_at(
            LogLevel::Warn,
            &format!(
                "rbox init: {} daemonized without PIDFile; daemon crash will not trigger Restart=on-failure",
                svc.name
            ),
        );
    }
}

/// 读取 PID 文件并解析 pid。
fn read_pid_file(path: &str) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()
}

/// 从单元配置构造运行时实例（公共字段）。
fn new_service_instance(
    unit: &Unit,
    cmd: &str,
    env: &[(String, String)],
    child: Option<Child>,
    tracked_pid: Option<u32>,
) -> ServiceInstance {
    ServiceInstance {
        name: unit.name.clone(),
        child,
        tracked_pid,
        exec_stop: unit.service.exec_stop.clone(),
        exec_reload: unit.service.exec_reload.clone(),
        exec_start: cmd.to_string(),
        env: env.to_vec(),
        restart_on_failure: unit.service.restart == "on-failure",
        restart_always: unit.service.restart == "always",
        restart_sec: unit.service.restart_sec,
        start_limit_burst: unit.service.start_limit_burst,
        start_limit_interval_sec: unit.service.start_limit_interval_sec,
        first_failure_at: None,
        fail_count: 0,
        next_restart_at: None,
        logfile: unit.service.logfile.clone(),
        user: unit.service.user.clone(),
        group: unit.service.group.clone(),
        is_forking: unit.service.typ == "forking",
        pidfile: unit.service.pidfile.clone(),
        timeout_start_sec: unit.service.timeout_start_sec,
        pgid: None,
        waiting_daemonize: false,
        daemonize_deadline: None,
        stopped: false,
    }
}

/// 服务退出后调度重启：失败计数 +1（成功退出清零），
/// 窗口内失败次数超过 StartLimitBurst 后放弃；距首次失败超过
/// StartLimitIntervalSec 则计数重置（时间窗）。失败时按 RestartSec 退避。
/// 策略：Restart=always 无论成败都重启；Restart=on-failure 仅失败重启。
pub(crate) fn schedule_restart(svc: &mut ServiceInstance, failed: bool) {
    let now = std::time::Instant::now();
    // 时间窗重置：距首次失败超过 interval 则清零计数
    if let Some(first) = svc.first_failure_at
        && now.duration_since(first) > std::time::Duration::from_secs(svc.start_limit_interval_sec)
    {
        svc.fail_count = 0;
        svc.first_failure_at = None;
    }
    if failed {
        if svc.fail_count == 0 {
            svc.first_failure_at = Some(now);
        }
        svc.fail_count += 1;
    } else {
        svc.fail_count = 0;
        svc.first_failure_at = None;
    }
    let want_restart = if svc.restart_always {
        true
    } else if svc.restart_on_failure {
        failed
    } else {
        false
    };
    if !want_restart || svc.stopped || shutdown_requested() {
        return;
    }
    if svc.fail_count > svc.start_limit_burst {
        log(&format!(
            "rbox init: {} failed {} times, giving up (StartLimitBurst={})",
            svc.name, svc.fail_count, svc.start_limit_burst
        ));
        return;
    }
    svc.next_restart_at = Some(now + std::time::Duration::from_secs(svc.restart_sec));
}

/// 重新拉起服务（自动重启与手动 start 共用）。
/// simple：直接 spawn；forking：spawn 父进程后异步等待 daemon 化（主循环处理）。
pub(crate) fn respawn_service(svc: &mut ServiceInstance) {
    let cfg = SpawnConfig {
        logfile: svc.logfile.as_deref(),
        user: svc.user.as_deref(),
        group: svc.group.as_deref(),
    };
    svc.tracked_pid = None;
    svc.child = spawn_unit_command(&svc.name, &svc.exec_start, &svc.env, &cfg);
    if !svc.is_forking {
        return;
    }
    // forking：spawn 父进程后异步等待 daemon 化（主循环按 TimeoutStartSec 处理超时）
    svc.pgid = svc.child.as_ref().map(|c| c.id());
    svc.waiting_daemonize = svc.child.is_some();
    svc.daemonize_deadline = if svc.waiting_daemonize {
        Some(std::time::Instant::now() + std::time::Duration::from_secs(svc.timeout_start_sec))
    } else {
        None
    };
}

/// ExecStop/ExecReload 命令超时（秒）。超时后 SIGKILL 该命令。
pub(crate) const EXEC_COMMAND_TIMEOUT: u64 = 5;

/// 运行一个命令并等待其退出，带超时（默认超时后 SIGKILL）。
/// 返回是否在超时内正常退出；argv 为空或 spawn 失败返回 false。
pub(crate) fn run_command_with_timeout(argv: &[String], timeout_secs: u64) -> bool {
    if argv.is_empty() {
        return false;
    }
    let mut child = match std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => return false,
        }
    }
}

/// 执行 ExecStop 并终止服务进程组：SIGTERM 等 1 秒，超时 SIGKILL。
/// 供关机流程与 stop/restart 命令复用；forking 服务额外终止 daemon pid。
pub(crate) fn stop_service_instance(svc: &mut ServiceInstance) {
    if let Some(stop_cmd) = &svc.exec_stop {
        log(&format!("rbox init: stopping {}: {}", svc.name, stop_cmd));
        let argv = parse_cmdline(stop_cmd);
        if !argv.is_empty() && !run_command_with_timeout(&argv, EXEC_COMMAND_TIMEOUT) {
            log_at(
                LogLevel::Warn,
                &format!("rbox init: ExecStop for {} timed out", svc.name),
            );
        }
    }
    if let Some(mut child) = svc.child.take() {
        let _ = kill_process_group(child.id(), libc::SIGTERM);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = kill_process_group(child.id(), libc::SIGKILL);
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(_) => break,
            }
        }
        let _ = child.wait();
    }
    // forking daemon：向 daemon pid 发信号并等待其退出（init 收养后 waitpid 可收割）
    if let Some(pid) = svc.tracked_pid.take() {
        let _ = kill_process(pid, libc::SIGTERM);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut status: libc::c_int = 0;
        loop {
            let r = unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) };
            if r == pid as i32 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = kill_process(pid, libc::SIGKILL);
                unsafe { libc::waitpid(pid as i32, &mut status, 0) };
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

/// 拉起降级/应急 shell（路径用配置的缺省 shell，默认 /bin/sh）。
pub(crate) fn spawn_fresh_shell() -> Option<Child> {
    let shell = crate::config::load().login.shell.clone();
    std::process::Command::new("/bin/rbox")
        .arg("sh")
        .spawn()
        .or_else(|_| std::process::Command::new(shell).spawn())
        .ok()
}

/// 日志轮转阈值（256 KB）。超过此大小则截断为空，避免 LogFile 无限增长。
const LOG_MAX_SIZE: u64 = 256 * 1024;

/// 检查日志文件大小，超过阈值则截断（轮转）。
/// 在每次服务 spawn 打开 LogFile 前调用。
fn rotate_log_if_needed(path: &str) {
    let Ok(meta) = std::fs::metadata(path) else {
        return; // 文件不存在，正常（首次写入会创建）
    };
    if meta.len() <= LOG_MAX_SIZE {
        return;
    }
    log_at(
        LogLevel::Info,
        &format!(
            "rbox init: rotating log {} ({}KB > {}KB)",
            path,
            meta.len() / 1024,
            LOG_MAX_SIZE / 1024
        ),
    );
    // 截断为空文件（简单轮转，不保留旧文件）
    if let Err(e) = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
    {
        log_at(
            LogLevel::Warn,
            &format!("rbox init: failed to rotate {}: {}", path, e),
        );
    }
}

/// 构造一个测试用 ServiceInstance（control.rs 的 format_status 测试共用）。
#[cfg(test)]
pub(crate) fn test_svc(name: &str, restart_on_failure: bool) -> ServiceInstance {
    ServiceInstance {
        name: name.to_string(),
        child: None,
        tracked_pid: None,
        exec_stop: None,
        exec_reload: None,
        exec_start: "/bin/rbox false".to_string(),
        env: Vec::new(),
        logfile: None,
        user: None,
        group: None,
        is_forking: false,
        pidfile: None,
        timeout_start_sec: 10,
        pgid: None,
        waiting_daemonize: false,
        daemonize_deadline: None,
        restart_on_failure,
        restart_always: false,
        restart_sec: 1,
        start_limit_burst: 5,
        start_limit_interval_sec: 10,
        first_failure_at: None,
        fail_count: 0,
        next_restart_at: None,
        stopped: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_environment_basic() {
        let env = parse_environment(&["A=1".into(), "B=two words".into()]);
        assert_eq!(
            env,
            vec![
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "two words".to_string())
            ]
        );
    }

    #[test]
    fn parse_environment_skips_invalid() {
        let env = parse_environment(&["A=1".into(), "NOEQUALS".into(), "=v".into()]);
        assert_eq!(env, vec![("A".to_string(), "1".to_string())]);
    }

    #[test]
    fn rotate_truncates_large_file() {
        let dir = format!("/tmp/rbox_rot_{}", std::process::id());
        let _ = std::fs::create_dir_all(&dir);
        let path = format!("{}/test.log", dir);
        std::fs::write(&path, "x".repeat((LOG_MAX_SIZE + 100) as usize)).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > LOG_MAX_SIZE);
        rotate_log_if_needed(&path);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotate_skips_small_file() {
        let dir = format!("/tmp/rbox_rot2_{}", std::process::id());
        let _ = std::fs::create_dir_all(&dir);
        let path = format!("{}/test.log", dir);
        std::fs::write(&path, "small").unwrap();
        rotate_log_if_needed(&path);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotate_skips_nonexistent() {
        rotate_log_if_needed("/tmp/rbox_nonexistent_log_file_xyz");
    }

    #[test]
    fn restart_burst_allows_exact_burst_then_gives_up() {
        let mut svc = test_svc("t.service", true);
        for attempt in 1..=6 {
            svc.next_restart_at = None;
            schedule_restart(&mut svc, true);
            if attempt <= 5 {
                assert!(
                    svc.next_restart_at.is_some(),
                    "attempt {attempt} should restart"
                );
            } else {
                assert!(
                    svc.next_restart_at.is_none(),
                    "attempt {attempt} should give up"
                );
            }
        }
        assert_eq!(svc.fail_count, 6);
    }

    #[test]
    fn restart_window_resets_after_interval() {
        let mut svc = test_svc("t.service", true);
        svc.fail_count = 5;
        svc.first_failure_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(11));
        svc.next_restart_at = None;
        schedule_restart(&mut svc, true);
        // 时间窗过期 → 计数重置 → 本次失败计数为 1（允许重启）
        assert_eq!(svc.fail_count, 1);
        assert!(svc.next_restart_at.is_some());
    }

    #[test]
    fn restart_success_clears_count() {
        let mut svc = test_svc("t.service", true);
        svc.fail_count = 3;
        svc.first_failure_at = Some(std::time::Instant::now());
        schedule_restart(&mut svc, false);
        assert_eq!(svc.fail_count, 0);
        assert!(svc.first_failure_at.is_none());
    }

    #[test]
    fn restart_always_restarts_on_success() {
        let mut svc = test_svc("t.service", false);
        svc.restart_always = true;
        schedule_restart(&mut svc, false); // 成功退出也重启
        assert!(svc.next_restart_at.is_some());
        assert_eq!(svc.fail_count, 0);
    }

    #[test]
    fn restart_always_respects_stopped() {
        let mut svc = test_svc("t.service", false);
        svc.restart_always = true;
        svc.stopped = true;
        schedule_restart(&mut svc, false);
        assert!(svc.next_restart_at.is_none());
    }

    #[test]
    fn restart_no_policy_never_restarts() {
        let mut svc = test_svc("t.service", false);
        schedule_restart(&mut svc, true); // 失败也不重启
        assert!(svc.next_restart_at.is_none());
    }

    #[test]
    fn finish_daemonize_reads_pidfile() {
        let dir = format!("/tmp/rbox_pid_{}", std::process::id());
        let _ = std::fs::create_dir_all(&dir);
        let pidfile = format!("{}/svc.pid", dir);
        std::fs::write(&pidfile, "4242\n").unwrap();
        let mut svc = test_svc("fork.service", true);
        svc.is_forking = true;
        svc.pidfile = Some(pidfile.clone());
        svc.waiting_daemonize = true;
        svc.daemonize_deadline = Some(std::time::Instant::now());
        finish_daemonize(&mut svc);
        assert_eq!(svc.tracked_pid, Some(4242));
        assert!(!svc.waiting_daemonize);
        assert!(svc.daemonize_deadline.is_none());
        assert!(svc.child.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finish_daemonize_without_pidfile_does_not_track() {
        let mut svc = test_svc("fork.service", true);
        svc.is_forking = true;
        svc.pidfile = None;
        svc.waiting_daemonize = true;
        svc.daemonize_deadline = Some(std::time::Instant::now());
        finish_daemonize(&mut svc);
        assert_eq!(svc.tracked_pid, None);
        assert!(!svc.waiting_daemonize);
    }
}
