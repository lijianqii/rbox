//! 单元配置：TOML 解析、单元名解析、依赖拓扑排序。

use crate::applets::core::{LogLevel, log_at};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// 解析后的单元文件（TOML 反序列化）。
/// TOML 表名使用 systemd 风格的 [Unit]/[Service]/[Install]。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Unit {
    #[serde(skip)]
    pub(crate) name: String,
    #[serde(skip)]
    pub(crate) is_target: bool,
    #[serde(default, rename = "Unit")]
    pub(crate) unit: UnitSection,
    #[serde(default, rename = "Service")]
    pub(crate) service: ServiceSection,
    #[serde(default, rename = "Install")]
    pub(crate) install: InstallSection,
    #[serde(default, rename = "Timer")]
    pub(crate) timer: TimerSection,
    #[serde(default, rename = "Path")]
    pub(crate) path: PathSection,
    #[serde(default, rename = "Socket")]
    pub(crate) socket: SocketSection,
    #[serde(skip)]
    pub(crate) is_timer: bool,
    #[serde(skip)]
    pub(crate) is_path: bool,
    #[serde(skip)]
    pub(crate) is_socket: bool,
    /// 模板单元（文件名含 `@`，如 `getty@.toml`）
    #[serde(skip)]
    pub(crate) is_template: bool,
}

/// `[Socket]`：socket 激活单元（`*.socket.toml`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct SocketSection {
    /// Unix 套接字路径（以 `/` 开头）或 TCP 端口（纯数字，绑定 127.0.0.1）
    #[serde(default, rename = "ListenStream")]
    pub(crate) listen_stream: Option<String>,
    /// 每个连接启动一次服务（连接作为 stdin/stdout）；缺省 false（监听 fd 作为 fd 3）
    #[serde(default, rename = "Accept")]
    pub(crate) accept: bool,
    /// 触发的服务单元（缺省为同名去掉 .socket）
    #[serde(default, rename = "Unit")]
    pub(crate) unit: Option<String>,
    /// Unix 套接字权限（八进制，如 0666）
    #[serde(default, rename = "SocketMode")]
    pub(crate) socket_mode: Option<u32>,
}

/// `[Timer]`：定时器单元（`*.timer.toml`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct TimerSection {
    /// 开机后 N 秒触发一次（支持 5s/2min/1h/1d 或纯秒数）
    #[serde(default, rename = "OnBootSec")]
    pub(crate) on_boot_sec: Option<String>,
    /// 定时器激活后 N 秒触发一次
    #[serde(default, rename = "OnActiveSec")]
    pub(crate) on_active_sec: Option<String>,
    /// 距上次触发 N 秒后重复触发
    #[serde(default, rename = "OnUnitActiveSec")]
    pub(crate) on_unit_active_sec: Option<String>,
    /// 简化日历：`HH:MM[:SS]`（每天）或 `*:0/N`（每 N 分钟）
    #[serde(default, rename = "OnCalendar")]
    pub(crate) on_calendar: Option<String>,
    /// 触发的服务单元（缺省为同名去掉 .timer）
    #[serde(default, rename = "Unit")]
    pub(crate) unit: Option<String>,
}

/// `[Path]`：路径监视单元（`*.path.toml`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct PathSection {
    #[serde(default, deserialize_with = "one_or_many")]
    #[serde(rename = "PathExists")]
    pub(crate) path_exists: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    #[serde(rename = "PathChanged")]
    pub(crate) path_changed: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    #[serde(rename = "DirectoryNotEmpty")]
    pub(crate) directory_not_empty: Vec<String>,
    /// 触发的服务单元（缺省为同名去掉 .path）
    #[serde(default, rename = "Unit")]
    pub(crate) unit: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct UnitSection {
    #[serde(default)]
    #[serde(rename = "Description")]
    pub(crate) description: String,
    /// 单元名：rservice/status/依赖引用均使用它；缺省回退文件名
    #[serde(default)]
    #[serde(rename = "Name")]
    pub(crate) name: String,
    #[serde(default)]
    #[serde(rename = "After")]
    pub(crate) after: Vec<String>,
    #[serde(default)]
    #[serde(rename = "Requires")]
    pub(crate) requires: Vec<String>,
    /// 尽力依赖：参与排序（先启动），但失败不阻止本单元
    #[serde(default)]
    #[serde(rename = "Wants")]
    pub(crate) wants: Vec<String>,
    /// 前置检查依赖：不激活依赖；启动前要求依赖已成功，否则本单元跳过
    #[serde(default)]
    #[serde(rename = "Requisite")]
    pub(crate) requisite: Vec<String>,
    /// 反向排序依赖：本单元必须先于这些单元启动（同 Before=）
    #[serde(default)]
    #[serde(rename = "Before")]
    pub(crate) before: Vec<String>,
    /// 互斥单元：启动本单元前停止它们（systemd Conflicts=）
    #[serde(default, deserialize_with = "one_or_many")]
    #[serde(rename = "Conflicts")]
    pub(crate) conflicts: Vec<String>,
    /// 联动单元：这些单元停止/重启时本单元随之停止/重启（PartOf=）
    #[serde(default, deserialize_with = "one_or_many")]
    #[serde(rename = "PartOf")]
    pub(crate) part_of: Vec<String>,
    /// 本单元失败时启动的单元（best-effort）
    #[serde(default, deserialize_with = "one_or_many")]
    #[serde(rename = "OnFailure")]
    pub(crate) on_failure: Vec<String>,
    /// 本单元成功退出时启动的单元
    #[serde(default, deserialize_with = "one_or_many")]
    #[serde(rename = "OnSuccess")]
    pub(crate) on_success: Vec<String>,
    /// 条件：路径存在才启动（不满足则跳过，不算失败）
    #[serde(default, deserialize_with = "one_or_many")]
    #[serde(rename = "ConditionPathExists")]
    pub(crate) condition_path_exists: Vec<String>,
    /// 条件：目录非空才启动
    #[serde(default, deserialize_with = "one_or_many")]
    #[serde(rename = "ConditionDirectoryNotEmpty")]
    pub(crate) condition_dir_not_empty: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ServiceSection {
    #[serde(default)]
    #[serde(rename = "Type")]
    pub(crate) typ: String,
    #[serde(default)]
    #[serde(rename = "ExecStart")]
    pub(crate) exec_start: Option<String>,
    #[serde(default)]
    #[serde(rename = "ExecStop")]
    pub(crate) exec_stop: Option<String>,
    /// rservice reload 时执行的命令
    #[serde(default)]
    #[serde(rename = "ExecReload")]
    pub(crate) exec_reload: Option<String>,
    /// 重启策略："" / "no"（默认）、"on-failure"（非零退出重启）或 "always"（退出即重启）
    #[serde(default)]
    #[serde(rename = "Restart")]
    pub(crate) restart: String,
    /// 自动重启间隔（秒，默认 1）
    #[serde(default = "default_restart_sec")]
    #[serde(rename = "RestartSec")]
    pub(crate) restart_sec: u64,
    /// 连续失败重启上限（默认 5，窗口内达到后停止重启）
    #[serde(default = "default_start_limit_burst")]
    #[serde(rename = "StartLimitBurst")]
    pub(crate) start_limit_burst: u32,
    /// 失败计数时间窗（秒，默认 10）：窗口内连续失败达 StartLimitBurst 后放弃，
    /// 距首次失败超过该时长则计数重置
    #[serde(default = "default_start_limit_interval")]
    #[serde(rename = "StartLimitIntervalSec")]
    pub(crate) start_limit_interval_sec: u64,
    /// Type=forking 时等待父进程退出的超时（秒，默认 10）
    #[serde(default = "default_timeout_start")]
    #[serde(rename = "TimeoutStartSec")]
    pub(crate) timeout_start_sec: u64,
    /// Type=forking 的 daemon PID 文件（可选）
    #[serde(default)]
    #[serde(rename = "PIDFile")]
    pub(crate) pidfile: Option<String>,
    /// 服务环境变量：["VAR=value", ...]
    #[serde(default)]
    #[serde(rename = "Environment")]
    pub(crate) environment: Vec<String>,
    /// 环境变量文件（每行 KEY=VALUE）；路径以 `-` 开头表示文件缺失不报错
    #[serde(default)]
    #[serde(rename = "EnvironmentFile")]
    pub(crate) environment_file: Option<String>,
    /// 服务工作目录（缺省继承 init 的 cwd）
    #[serde(default)]
    #[serde(rename = "WorkingDirectory")]
    pub(crate) working_directory: Option<String>,
    /// 停止超时秒数（SIGTERM 后等待时间，超时 SIGKILL；默认 5）
    #[serde(default = "default_timeout_stop")]
    #[serde(rename = "TimeoutStopSec")]
    pub(crate) timeout_stop_sec: u64,
    /// 停止模式：control-group（默认，杀整个进程组）/ process / mixed / none
    #[serde(default = "default_kill_mode")]
    #[serde(rename = "KillMode")]
    pub(crate) kill_mode: String,
    /// stdout/stderr 重定向文件（可选）
    #[serde(default)]
    #[serde(rename = "LogFile")]
    pub(crate) logfile: Option<String>,
    /// 以指定用户/组运行（可选）
    #[serde(default)]
    #[serde(rename = "User")]
    pub(crate) user: Option<String>,
    #[serde(default)]
    #[serde(rename = "Group")]
    pub(crate) group: Option<String>,
    /// 启动前依次同步执行的命令；任一失败则启动失败
    #[serde(default, deserialize_with = "one_or_many")]
    #[serde(rename = "ExecStartPre")]
    pub(crate) exec_start_pre: Vec<String>,
    /// 启动成功后依次同步执行的命令
    #[serde(default, deserialize_with = "one_or_many")]
    #[serde(rename = "ExecStartPost")]
    pub(crate) exec_start_post: Vec<String>,
    /// 停止后执行的命令（无论停止是否成功）
    #[serde(default, deserialize_with = "one_or_many")]
    #[serde(rename = "ExecStopPost")]
    pub(crate) exec_stop_post: Vec<String>,
    /// Type=oneshot 且进程退出后仍视为 active
    #[serde(default)]
    #[serde(rename = "RemainAfterExit")]
    pub(crate) remain_after_exit: bool,
    /// 视为成功的额外退出码（默认仅 0）
    #[serde(default, deserialize_with = "one_or_many_i32")]
    #[serde(rename = "SuccessExitStatus")]
    pub(crate) success_exit_status: Vec<i32>,
    /// 停止时发送的信号名（默认 TERM）
    #[serde(default)]
    #[serde(rename = "KillSignal")]
    pub(crate) kill_signal: Option<String>,
    /// 停止超时后是否补发 SIGKILL（默认 true）
    #[serde(default = "default_true")]
    #[serde(rename = "SendSIGKILL")]
    pub(crate) send_sigkill: bool,
    /// 进程属性：umask（八进制，如 0o027）
    #[serde(default, rename = "UMask")]
    pub(crate) umask: Option<u32>,
    /// 进程优先级（-20..19）
    #[serde(default, rename = "Nice")]
    pub(crate) nice: Option<i32>,
    /// OOM 评分调整（-1000..1000）
    #[serde(default, rename = "OOMScoreAdjust")]
    pub(crate) oom_score_adjust: Option<i32>,
    /// 资源限制（软/硬同值）：LimitNOFILE/LimitNPROC/LimitCORE/LimitAS
    #[serde(default, rename = "LimitNOFILE")]
    pub(crate) limit_nofile: Option<u64>,
    #[serde(default, rename = "LimitNPROC")]
    pub(crate) limit_nproc: Option<u64>,
    #[serde(default, rename = "LimitCORE")]
    pub(crate) limit_core: Option<u64>,
    #[serde(default, rename = "LimitAS")]
    pub(crate) limit_as: Option<u64>,
    /// 沙箱：禁止提权（PR_SET_NO_NEW_PRIVS）
    #[serde(default, rename = "NoNewPrivileges")]
    pub(crate) no_new_privileges: bool,
    /// 沙箱：私有 /tmp 与 /var/tmp（mount namespace + tmpfs）
    #[serde(default, rename = "PrivateTmp")]
    pub(crate) private_tmp: bool,
    /// 沙箱：隐藏 /home /root /run/user（yes/read-only/tmpfs）
    #[serde(default, rename = "ProtectHome")]
    pub(crate) protect_home: Option<String>,
    /// 沙箱：只读挂载系统目录（yes/full/strict）
    #[serde(default, rename = "ProtectSystem")]
    pub(crate) protect_system: Option<String>,
    /// cgroup v2：内存上限（64M/1G/纯字节）
    #[serde(default, rename = "MemoryMax")]
    pub(crate) memory_max: Option<String>,
    /// cgroup v2：CPU 配额（如 50%）
    #[serde(default, rename = "CPUQuota")]
    pub(crate) cpu_quota: Option<String>,
    /// cgroup v2：CPU 权重（1..10000，默认 100）
    #[serde(default, rename = "CPUWeight")]
    pub(crate) cpu_weight: Option<u64>,
    /// cgroup v2：进程数上限
    #[serde(default, rename = "TasksMax")]
    pub(crate) tasks_max: Option<u64>,
    /// cgroup v2：所属 slice（默认 system.slice）
    #[serde(default, rename = "Slice")]
    pub(crate) slice: Option<String>,
    /// 看门狗超时（Type=notify 服务需周期发送 WATCHDOG=1）
    #[serde(default, rename = "WatchdogSec")]
    pub(crate) watchdog_sec: Option<String>,
}

fn default_restart_sec() -> u64 {
    1
}
fn default_start_limit_burst() -> u32 {
    5
}
fn default_start_limit_interval() -> u64 {
    10
}
fn default_timeout_start() -> u64 {
    10
}
fn default_timeout_stop() -> u64 {
    5
}
fn default_kill_mode() -> String {
    "control-group".to_string()
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct InstallSection {
    #[serde(default)]
    #[serde(rename = "WantedBy")]
    pub(crate) wanted_by: Vec<String>,
}

/// 合并 `<unit>.d/*.toml` drop-in 覆盖（深合并，后加载覆盖先加载）。
pub(crate) fn apply_dropins(path: &Path, base: &str) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = path.with_file_name(format!("{}.d", stem));
    let Ok(entries) = fs::read_dir(&dir) else {
        return base.to_string();
    };
    let mut value: toml::Value = match base.parse() {
        Ok(v) => v,
        Err(_) => return base.to_string(),
    };
    let mut files: Vec<std::path::PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toml"))
        .collect();
    files.sort();
    for f in files {
        let Ok(content) = fs::read_to_string(&f) else {
            continue;
        };
        let Ok(over) = content.parse::<toml::Value>() else {
            log_at(
                LogLevel::Warn,
                &format!("rbox init: parse error in drop-in {}", f.display()),
            );
            continue;
        };
        merge_toml(&mut value, over);
    }
    value.to_string()
}

/// 深合并 TOML（表递归合并，其余覆盖）。
fn merge_toml(base: &mut toml::Value, over: toml::Value) {
    match (base, over) {
        (toml::Value::Table(b), toml::Value::Table(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(bv) => merge_toml(bv, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

/// 扫描单元目录，返回 (unit_name, 路径, 是否启用) 列表（含 `*.toml.disabled`）。
pub(crate) fn scan_unit_files() -> Vec<(String, std::path::PathBuf, bool)> {
    let mut out = Vec::new();
    let dir = Path::new(&crate::config::load().paths.system_dir);
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let fname = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (enabled, stem) = if let Some(stem) = fname.strip_suffix(".toml.disabled") {
            (false, stem.to_string())
        } else if let Some(stem) = fname.strip_suffix(".toml") {
            (true, stem.to_string())
        } else {
            continue;
        };
        let name = fs::read_to_string(&path)
            .ok()
            .and_then(|c| toml::from_str::<Unit>(&c).ok())
            .map(|u| resolve_unit_name(&stem, &u.unit.name))
            .unwrap_or(stem);
        out.push((name, path, enabled));
    }
    out
}

/// 接受单个字符串或字符串数组（TOML 中 `X = "a"` 与 `X = ["a"]` 均可）。
fn one_or_many<'de, D>(de: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(de)? {
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
    })
}

/// 接受单个整数或整数数组（SuccessExitStatus）。
fn one_or_many_i32<'de, D>(de: D) -> Result<Vec<i32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(i32),
        Many(Vec<i32>),
    }
    Ok(match OneOrMany::deserialize(de)? {
        OneOrMany::One(n) => vec![n],
        OneOrMany::Many(v) => v,
    })
}

/// 条件检查
pub(crate) fn resolve_unit_name(file_stem: &str, declared: &str) -> String {
    if declared.is_empty() {
        file_stem.to_string()
    } else {
        declared.to_string()
    }
}

/// 是否为模板单元（文件名含 `@`）。
pub(crate) fn is_template_file(file_stem: &str) -> bool {
    file_stem.contains('@')
}

/// 展开 systemd 风格说明符：`%i` 实例、`%I` 实例（`-`→`/`）、`%n` 完整单元名、
/// `%N` 前缀、`%p` 前缀（同 `%N`）、`%u` 用户名、`%h` HOME、`%%` 字面 `%`。
pub(crate) fn expand_specifiers(
    spec: &str,
    prefix: &str,
    instance: &str,
    full_name: &str,
) -> String {
    let mut out = String::with_capacity(spec.len());
    let mut chars = spec.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('i') => out.push_str(instance),
            Some('I') => out.push_str(&instance.replace('-', "/")),
            Some('n') => out.push_str(full_name),
            Some('N') | Some('p') => out.push_str(prefix),
            Some('u') => {
                out.push_str(&std::env::var("USER").unwrap_or_else(|_| "root".to_string()))
            }
            Some('h') => {
                out.push_str(&std::env::var("HOME").unwrap_or_else(|_| "/root".to_string()))
            }
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// 对单元所有字符串字段展开说明符。
pub(crate) fn expand_unit_specifiers(unit: &mut Unit, prefix: &str, instance: &str) {
    let full = unit.name.clone();
    let ex = |s: &String| expand_specifiers(s, prefix, instance, &full);
    let exo = |o: &Option<String>| {
        o.as_ref()
            .map(|s| expand_specifiers(s, prefix, instance, &full))
    };
    let exv = |v: &Vec<String>| v.iter().map(ex).collect::<Vec<_>>();
    unit.unit.description = ex(&unit.unit.description);
    unit.unit.after = exv(&unit.unit.after);
    unit.unit.before = exv(&unit.unit.before);
    unit.unit.requires = exv(&unit.unit.requires);
    unit.unit.wants = exv(&unit.unit.wants);
    unit.unit.requisite = exv(&unit.unit.requisite);
    unit.unit.conflicts = exv(&unit.unit.conflicts);
    unit.unit.part_of = exv(&unit.unit.part_of);
    unit.unit.on_failure = exv(&unit.unit.on_failure);
    unit.unit.on_success = exv(&unit.unit.on_success);
    unit.unit.condition_path_exists = exv(&unit.unit.condition_path_exists);
    unit.unit.condition_dir_not_empty = exv(&unit.unit.condition_dir_not_empty);
    unit.service.exec_start = exo(&unit.service.exec_start);
    unit.service.exec_stop = exo(&unit.service.exec_stop);
    unit.service.exec_reload = exo(&unit.service.exec_reload);
    unit.service.exec_start_pre = exv(&unit.service.exec_start_pre);
    unit.service.exec_start_post = exv(&unit.service.exec_start_post);
    unit.service.exec_stop_post = exv(&unit.service.exec_stop_post);
    unit.service.environment = exv(&unit.service.environment);
    unit.service.environment_file = exo(&unit.service.environment_file);
    unit.service.working_directory = exo(&unit.service.working_directory);
    unit.service.pidfile = exo(&unit.service.pidfile);
    unit.service.logfile = exo(&unit.service.logfile);
    unit.install.wanted_by = exv(&unit.install.wanted_by);
}

/// 由模板实例化单元：`full_name` 形如 `prefix@instance`。
pub(crate) fn instantiate_template(template: &Unit, full_name: &str) -> Option<Unit> {
    let (prefix, instance) = full_name.split_once('@')?;
    if prefix.is_empty() || instance.is_empty() {
        return None;
    }
    let mut u = template.clone();
    u.name = full_name.to_string();
    u.is_template = false;
    expand_unit_specifiers(&mut u, prefix, instance);
    Some(u)
}

/// 是否为 target 单元（按文件名 .target 后缀判定，与 Name 字段无关）。
pub(crate) fn is_target_file(file_stem: &str) -> bool {
    file_stem.ends_with(".target")
}

/// 是否为 timer 单元（`*.timer`）。
pub(crate) fn is_timer_file(file_stem: &str) -> bool {
    file_stem.ends_with(".timer")
}

/// 是否为 path 单元（`*.path`）。
pub(crate) fn is_path_file(file_stem: &str) -> bool {
    file_stem.ends_with(".path")
}

/// 是否为 socket 单元（`*.socket`）。
pub(crate) fn is_socket_file(file_stem: &str) -> bool {
    file_stem.ends_with(".socket")
}

/// 解析 systemd 风格时长（`5s`/`2min`/`1h`/`1d`/纯秒数）。
pub(crate) fn parse_duration(spec: &str) -> Option<u64> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    if let Ok(n) = spec.parse::<u64>() {
        return Some(n);
    }
    for (suffix, mul) in [
        ("seconds", 1u64),
        ("second", 1),
        ("secs", 1),
        ("sec", 1),
        ("minutes", 60),
        ("minute", 60),
        ("mins", 60),
        ("min", 60),
        ("hours", 3600),
        ("hour", 3600),
        ("hrs", 3600),
        ("hr", 3600),
        ("days", 86400),
        ("day", 86400),
        ("d", 86400),
        ("h", 3600),
        ("m", 60),
        ("s", 1),
        ("ms", 0),
    ] {
        if let Some(num) = spec.strip_suffix(suffix)
            && let Ok(n) = num.trim().parse::<u64>()
        {
            return Some(n.saturating_mul(mul) / if suffix == "ms" { 1000 } else { 1 });
        }
    }
    None
}

/// 加载单元目录（路径可配置，见 /etc/rbox.conf [paths] system_dir）下所有 .toml 单元文件。
pub(crate) fn load_all_units() -> std::io::Result<HashMap<String, Unit>> {
    let mut units: HashMap<String, Unit> = HashMap::new();
    let primary = crate::config::load().paths.system_dir.clone();
    // 搜索路径优先级：/etc（配置）> /run > /usr/lib；低优先级先加载，高优先级覆盖
    let dirs: Vec<String> = vec![
        "/usr/lib/rbox/system".to_string(),
        "/run/rbox/system".to_string(),
        primary.clone(),
    ];
    if !Path::new(&primary).exists() {
        // 目录缺失（如测试模式/配置错误）：告警避免"无服务也能正常启动"的假象
        log_at(
            LogLevel::Warn,
            &format!("rbox init: unit dir {} not found", primary),
        );
    }
    for dir in dirs.iter().map(|d| Path::new(d.as_str())) {
        if !dir.exists() {
            continue;
        }
        load_dir_into(dir, &mut units);
    }
    // 模板实例化：收集所有引用（依赖/WantedBy/联动），出现 prefix@instance 且模板存在则实例化
    instantiate_referenced_templates(&mut units);
    Ok(units)
}

/// 加载单个目录的单元（高优先级目录后加载，覆盖低优先级同名单元）。
/// 指向 /dev/null 的符号链接表示 mask：从表中移除该单元。
fn load_dir_into(dir: &Path, units: &mut HashMap<String, Unit>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("toml") {
            // mask：符号链接指向 /dev/null → 移除该单元
            if let Ok(target) = fs::read_link(&path)
                && target == Path::new("/dev/null")
            {
                let stem = path
                    .file_stem()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                units.remove(&stem);
                continue;
            }
            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    log_at(
                        LogLevel::Warn,
                        &format!("rbox init: cannot read {}: {}", path.display(), e),
                    );
                    continue;
                }
            };
            let content = apply_dropins(&path, &content);
            match toml::from_str::<Unit>(&content) {
                Ok(mut unit) => {
                    // 文件名去掉 .toml；单元名优先用 [Unit] Name，缺省回退文件名
                    let file_stem = path
                        .file_stem()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    unit.is_target = is_target_file(&file_stem);
                    unit.is_timer = is_timer_file(&file_stem);
                    unit.is_path = is_path_file(&file_stem);
                    unit.is_socket = is_socket_file(&file_stem);
                    unit.is_template = is_template_file(&file_stem);
                    if !unit.is_template {
                        let (prefix, instance) = match unit.name.split_once('@') {
                            Some((p, i)) => (p.to_string(), i.to_string()),
                            None => (unit.name.clone(), String::new()),
                        };
                        expand_unit_specifiers(&mut unit, &prefix, &instance);
                    }
                    unit.name = resolve_unit_name(&file_stem, &unit.unit.name);
                    if units.contains_key(&unit.name) {
                        log_at(
                            LogLevel::Warn,
                            &format!(
                                "rbox init: duplicate unit name '{}' in {}, overriding earlier",
                                unit.name,
                                path.display()
                            ),
                        );
                    }
                    units.insert(unit.name.clone(), unit);
                }
                Err(e) => {
                    log_at(
                        LogLevel::Warn,
                        &format!("rbox init: parse error in {}: {}", path.display(), e),
                    );
                }
            }
        }
    }
    // （模板实例化在 load_all_units 末尾统一处理）
    let _ = units;
}

/// 收集所有引用并实例化 `prefix@instance`（模板存在时）。
fn instantiate_referenced_templates(units: &mut HashMap<String, Unit>) {
    let mut refs: std::collections::HashSet<String> = std::collections::HashSet::new();
    for u in units.values() {
        for list in [
            &u.unit.after,
            &u.unit.before,
            &u.unit.requires,
            &u.unit.wants,
            &u.unit.requisite,
            &u.unit.conflicts,
            &u.unit.part_of,
            &u.unit.on_failure,
            &u.unit.on_success,
            &u.install.wanted_by,
        ] {
            for r in list {
                refs.insert(r.clone());
            }
        }
    }
    for r in refs {
        if units.contains_key(&r) {
            continue;
        }
        let Some((prefix, _)) = r.split_once('@') else {
            continue;
        };
        let tname = format!("{}@", prefix);
        let Some(t) = units.get(&tname).cloned() else {
            continue;
        };
        if !t.is_template {
            continue;
        }
        if let Some(inst) = instantiate_template(&t, &r) {
            units.insert(r.clone(), inst);
        }
    }
}

/// 计算单元的排序依赖：Requires/After/Wants + 反向 Before +
/// target 的 WantedBy 反边（声明 Before=name 的单元必须先启动）。
pub(crate) fn sort_deps(name: &str, unit: &Unit, units: &HashMap<String, Unit>) -> Vec<String> {
    let mut deps = unit.unit.requires.clone();
    deps.extend(unit.unit.after.iter().cloned());
    deps.extend(unit.unit.wants.iter().cloned());
    // Before= 反边：其他单元声明 Before=本单元 -> 先启动它们
    for (other_name, other) in units.iter() {
        if other.unit.before.iter().any(|b| b == name) {
            deps.push(other_name.clone());
        }
    }
    if unit.is_target {
        for (other_name, other) in units.iter() {
            if other.install.wanted_by.iter().any(|w| w == name) {
                deps.push(other_name.clone());
            }
        }
    }
    deps
}

/// 从 default.target 出发，计算服务的启动顺序（拓扑排序）。
/// Requires= 和 After= 都构成"必须先启动"的边。
pub(crate) fn compute_start_order(
    units: &HashMap<String, Unit>,
    root: &str,
) -> Result<Vec<String>, String> {
    let mut order = Vec::new();
    let mut visited: HashMap<String, u8> = HashMap::new(); // 0=未访问 1=进行中 2=已完成

    fn visit(
        name: &str,
        units: &HashMap<String, Unit>,
        order: &mut Vec<String>,
        visited: &mut HashMap<String, u8>,
    ) -> Result<(), String> {
        let st = *visited.entry(name.to_string()).or_insert(0);
        match st {
            2 => return Ok(()),
            1 => return Err(format!("cycle detected at {}", name)),
            _ => {}
        }
        visited.insert(name.to_string(), 1);

        let unit = match units.get(name) {
            Some(u) => u,
            None => {
                // 缺失依赖：告警但不中断（与 systemd 的宽松行为一致）
                log_at(
                    LogLevel::Warn,
                    &format!("rbox init: dependency '{}' not found", name),
                );
                visited.insert(name.to_string(), 2);
                return Ok(());
            }
        };

        // Wants：尽力依赖，参与排序（先启动）但失败不传播；
        // Requisite：不参与排序（不激活依赖），仅在启动前检查状态；
        // Before：反向边（其他单元声明 Before=name 时先启动它们）；
        // target 节点：把所有 WantedBy=该 target 的服务拉进来（反向依赖）
        for dep in sort_deps(name, unit, units) {
            visit(&dep, units, order, visited)?;
        }

        order.push(name.to_string());
        visited.insert(name.to_string(), 2);
        Ok(())
    }

    visit(root, units, &mut order, &mut visited)?;
    Ok(order)
}

/// 将命令字符串切分为 argv。
/// 支持双引号、单引号、反斜杠转义；空格/制表符分隔（引号内保留）。
/// 单引号内所有字符字面（反斜杠不转义）；双引号内与引号外反斜杠转义下一字符。
pub(crate) fn parse_cmdline(s: &str) -> Vec<String> {
    let mut argv = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None; // 当前引号：'"' 或 '\''
    let mut escaped = false;
    for c in s.chars() {
        if escaped {
            cur.push(c);
            escaped = false;
            continue;
        }
        match c {
            // 反斜杠转义下一字符（单引号内除外，遵循 shell 语义）
            '\\' if quote != Some('\'') => escaped = true,
            '\'' => match quote {
                Some('\'') => quote = None,
                None => quote = Some('\''),
                Some(_) => cur.push(c),
            },
            '"' => match quote {
                Some('"') => quote = None,
                None => quote = Some('"'),
                Some(_) => cur.push(c),
            },
            ' ' | '\t' if quote.is_none() => {
                if !cur.is_empty() {
                    argv.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        argv.push(cur);
    }
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个测试用 Unit。
    fn unit(
        name: &str,
        is_target: bool,
        requires: &[&str],
        after: &[&str],
        wanted_by: &[&str],
    ) -> Unit {
        Unit {
            name: name.to_string(),
            is_target,
            unit: UnitSection {
                after: after.iter().map(|s| s.to_string()).collect(),
                requires: requires.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            service: ServiceSection {
                typ: "simple".to_string(),
                restart_sec: 1,
                start_limit_burst: 5,
                start_limit_interval_sec: 10,
                timeout_start_sec: 10,
                timeout_stop_sec: 5,
                kill_mode: "control-group".to_string(),
                send_sigkill: true,
                ..Default::default()
            },
            install: InstallSection {
                wanted_by: wanted_by.iter().map(|s| s.to_string()).collect(),
            },
            ..Default::default()
        }
    }

    #[test]
    fn parse_cmdline_basic() {
        assert_eq!(
            parse_cmdline("/bin/rbox echo hello"),
            vec!["/bin/rbox", "echo", "hello"]
        );
    }

    #[test]
    fn parse_cmdline_quotes() {
        assert_eq!(
            parse_cmdline("/bin/rbox echo \"hello world\""),
            vec!["/bin/rbox", "echo", "hello world"]
        );
    }

    #[test]
    fn parse_cmdline_ignores_extra_spaces() {
        assert_eq!(parse_cmdline("  a   b  "), vec!["a", "b"]);
    }

    #[test]
    fn parse_cmdline_single_quotes() {
        assert_eq!(
            parse_cmdline("/bin/rbox echo 'hello world'"),
            vec!["/bin/rbox", "echo", "hello world"]
        );
    }

    #[test]
    fn parse_cmdline_backslash_escape() {
        assert_eq!(
            parse_cmdline("/bin/rbox echo hello\\ world"),
            vec!["/bin/rbox", "echo", "hello world"]
        );
    }

    #[test]
    fn parse_cmdline_backslash_in_single_quotes_is_literal() {
        // 单引号内反斜杠不转义，按 shell 语义原样保留
        assert_eq!(
            parse_cmdline("/bin/rbox echo 'a\\b'"),
            vec!["/bin/rbox", "echo", "a\\b"]
        );
    }

    #[test]
    fn parse_cmdline_mixed_quotes() {
        assert_eq!(
            parse_cmdline("/bin/rbox echo \"a'b\" 'c\"d'"),
            vec!["/bin/rbox", "echo", "a'b", "c\"d"]
        );
    }

    #[test]
    fn parse_cmdline_tab_separator() {
        assert_eq!(parse_cmdline("a\tb"), vec!["a", "b"]);
    }

    #[test]
    fn start_order_respects_requires() {
        let mut units = HashMap::new();
        units.insert(
            "default.target".into(),
            unit("default.target", true, &["b.service"], &[], &[]),
        );
        units.insert(
            "b.service".into(),
            unit("b.service", false, &["a.service"], &[], &[]),
        );
        units.insert("a.service".into(), unit("a.service", false, &[], &[], &[]));
        let order = compute_start_order(&units, "default.target").unwrap();
        assert_eq!(order, vec!["a.service", "b.service", "default.target"]);
    }

    #[test]
    fn start_order_respects_after() {
        let mut units = HashMap::new();
        units.insert(
            "default.target".into(),
            unit("default.target", true, &[], &["a.service"], &[]),
        );
        units.insert("a.service".into(), unit("a.service", false, &[], &[], &[]));
        let order = compute_start_order(&units, "default.target").unwrap();
        assert_eq!(order, vec!["a.service", "default.target"]);
    }

    #[test]
    fn start_order_detects_cycle() {
        let mut units = HashMap::new();
        units.insert(
            "a.service".into(),
            unit("a.service", false, &["b.service"], &[], &[]),
        );
        units.insert(
            "b.service".into(),
            unit("b.service", false, &["a.service"], &[], &[]),
        );
        let err = compute_start_order(&units, "a.service").unwrap_err();
        assert!(err.contains("cycle"), "unexpected error: {}", err);
    }

    #[test]
    fn start_order_pulls_wantedby_services() {
        let mut units = HashMap::new();
        units.insert(
            "default.target".into(),
            unit("default.target", true, &[], &[], &[]),
        );
        units.insert(
            "svc.service".into(),
            unit("svc.service", false, &[], &[], &["default.target"]),
        );
        let order = compute_start_order(&units, "default.target").unwrap();
        // default.target 必须是最后一个（DFS 后序）
        assert_eq!(order.last().map(String::as_str), Some("default.target"));
        // WantedBy 的服务必须被拉入且排在 target 之前
        let i_svc = order.iter().position(|n| n == "svc.service").unwrap();
        let i_def = order.iter().position(|n| n == "default.target").unwrap();
        assert!(i_svc < i_def);
    }

    #[test]
    fn start_order_missing_root_is_ok() {
        let units: HashMap<String, Unit> = HashMap::new();
        assert!(compute_start_order(&units, "ghost.target").is_ok());
    }

    #[test]
    fn start_order_respects_wants() {
        // Wants 参与排序（先启动），但不要求成功
        let mut units = HashMap::new();
        let mut t = unit("default.target", true, &[], &[], &[]);
        t.unit.wants = vec!["a.service".to_string()];
        units.insert("default.target".into(), t);
        units.insert("a.service".into(), unit("a.service", false, &[], &[], &[]));
        let order = compute_start_order(&units, "default.target").unwrap();
        assert_eq!(order, vec!["a.service", "default.target"]);
    }

    #[test]
    fn start_order_ignores_requisite() {
        // Requisite 不激活依赖：不参与拓扑排序，仅启动前检查状态
        let mut units = HashMap::new();
        let mut t = unit("default.target", true, &[], &[], &[]);
        t.unit.requisite = vec!["b.service".to_string()];
        units.insert("default.target".into(), t);
        units.insert("b.service".into(), unit("b.service", false, &[], &[], &[]));
        let order = compute_start_order(&units, "default.target").unwrap();
        // b.service 未被激活，不在启动顺序中
        assert_eq!(order, vec!["default.target"]);
    }

    #[test]
    fn resolve_unit_name_prefers_declared() {
        assert_eq!(resolve_unit_name("hello.service", ""), "hello.service");
        assert_eq!(resolve_unit_name("hello.service", "hello"), "hello");
    }

    #[test]
    fn is_target_file_uses_filename_suffix() {
        assert!(is_target_file("default.target"));
        assert!(!is_target_file("default"));
        assert!(!is_target_file("hello.service"));
    }

    #[test]
    fn service_exec_start_keeps_full_command() {
        // getty 参数（-L/-t/tty）应直接写在 ExecStart 完整命令中，init 不做额外字段。
        let u: Unit =
            toml::from_str("[Service]\nExecStart = \"/bin/rgetty -L -t 60 ttyAMA0\"\n").unwrap();
        assert_eq!(
            u.service.exec_start.as_deref(),
            Some("/bin/rgetty -L -t 60 ttyAMA0")
        );
    }

    #[test]
    fn parse_new_service_and_unit_fields() {
        let toml_src = r#"
[Unit]
Name = "demo"
Conflicts = ["old"]
PartOf = ["grp"]
OnFailure = ["rescue"]
OnSuccess = ["notify"]
ConditionPathExists = ["/etc/passwd", "!/nonexistent"]

[Service]
Type = "oneshot"
ExecStart = "/bin/true"
ExecStartPre = ["/bin/echo pre"]
ExecStartPost = ["/bin/echo post"]
ExecStopPost = ["/bin/echo stop-post"]
RemainAfterExit = true
SuccessExitStatus = [2, 3]
KillSignal = "INT"
SendSIGKILL = false

[Install]
WantedBy = ["multi-user.target"]
"#;
        let u: Unit = toml::from_str(toml_src).unwrap();
        assert_eq!(u.unit.conflicts, vec!["old"]);
        assert_eq!(u.unit.part_of, vec!["grp"]);
        assert_eq!(u.unit.on_failure, vec!["rescue"]);
        assert_eq!(u.unit.on_success, vec!["notify"]);
        assert_eq!(u.unit.condition_path_exists.len(), 2);
        assert_eq!(u.service.typ, "oneshot");
        assert_eq!(u.service.exec_start_pre, vec!["/bin/echo pre"]);
        assert_eq!(u.service.exec_start_post, vec!["/bin/echo post"]);
        assert_eq!(u.service.exec_stop_post, vec!["/bin/echo stop-post"]);
        assert!(u.service.remain_after_exit);
        assert_eq!(u.service.success_exit_status, vec![2, 3]);
        assert_eq!(u.service.kill_signal.as_deref(), Some("INT"));
        assert!(!u.service.send_sigkill);
    }

    #[test]
    fn specifier_expansion() {
        assert_eq!(
            expand_specifiers("getty@%i %n %N %%", "getty", "tty1", "getty@tty1"),
            "getty@tty1 getty@tty1 getty %"
        );
        assert_eq!(
            expand_specifiers("%I", "a", "foo-bar", "a@foo-bar"),
            "foo/bar"
        );
        // 未知说明符原样保留
        assert_eq!(expand_specifiers("%z", "a", "b", "a@b"), "%z");
        assert_eq!(expand_specifiers("no-spec", "a", "b", "a@b"), "no-spec");
    }

    #[test]
    fn template_instantiation() {
        let mut t = Unit {
            name: "getty@".to_string(),
            is_template: true,
            ..Default::default()
        };
        t.service.exec_start = Some("/bin/rbox echo getty@%i".to_string());
        t.service.environment = vec!["TTY=%i".to_string()];
        t.install.wanted_by = vec!["default.target".to_string()];
        let inst = instantiate_template(&t, "getty@tty1").unwrap();
        assert_eq!(inst.name, "getty@tty1");
        assert!(!inst.is_template);
        assert_eq!(
            inst.service.exec_start.as_deref(),
            Some("/bin/rbox echo getty@tty1")
        );
        assert_eq!(inst.service.environment, vec!["TTY=tty1"]);
        // 非法实例名
        assert!(instantiate_template(&t, "getty@").is_none());
        assert!(instantiate_template(&t, "getty").is_none());
    }

    #[test]
    fn load_dir_mask_removes_unit() {
        let dir = std::path::PathBuf::from(format!("/tmp/rbox_units_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("masked.toml"),
            "[Service]\nExecStart = \"/bin/true\"\n",
        )
        .unwrap();
        let mut units = std::collections::HashMap::new();
        load_dir_into(&dir, &mut units);
        assert!(units.contains_key("masked"));
        // 覆盖为指向 /dev/null 的符号链接 = mask
        std::fs::remove_file(dir.join("masked.toml")).unwrap();
        std::os::unix::fs::symlink("/dev/null", dir.join("masked.toml")).unwrap();
        load_dir_into(&dir, &mut units);
        assert!(!units.contains_key("masked"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
