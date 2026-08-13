//! 单元配置：TOML 解析、单元名解析、依赖拓扑排序。

use crate::applets::core::{LogLevel, log_at};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

pub(crate) const SYSTEM_DIR: &str = "/etc/rbox/system";
pub(crate) const DEFAULT_TARGET: &str = "default.target";

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
    /// 重启策略："" / "no"（默认）或 "on-failure"
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
    /// 前台 console 服务（如交互 shell），退出后自动 respawn
    #[serde(default)]
    #[serde(rename = "Console")]
    pub(crate) console: bool,
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

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct InstallSection {
    #[serde(default)]
    #[serde(rename = "WantedBy")]
    pub(crate) wanted_by: Vec<String>,
}

/// 单元名解析：优先 [Unit] Name 字段，缺省回退文件名（去掉 .toml）。
pub(crate) fn resolve_unit_name(file_stem: &str, declared: &str) -> String {
    if declared.is_empty() {
        file_stem.to_string()
    } else {
        declared.to_string()
    }
}

/// 是否为 target 单元（按文件名 .target 后缀判定，与 Name 字段无关）。
pub(crate) fn is_target_file(file_stem: &str) -> bool {
    file_stem.ends_with(".target")
}

/// 加载 SYSTEM_DIR 下所有 .toml 单元文件。
pub(crate) fn load_all_units() -> std::io::Result<HashMap<String, Unit>> {
    let mut units: HashMap<String, Unit> = HashMap::new();
    let dir = Path::new(SYSTEM_DIR);
    if !dir.exists() {
        return Ok(units);
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("toml") {
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
            match toml::from_str::<Unit>(&content) {
                Ok(mut unit) => {
                    // 文件名去掉 .toml；单元名优先用 [Unit] Name，缺省回退文件名
                    let file_stem = path
                        .file_stem()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    unit.is_target = is_target_file(&file_stem);
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
    Ok(units)
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

        let mut deps = unit.unit.requires.clone();
        deps.extend(unit.unit.after.iter().cloned());
        // target 节点：把所有 WantedBy=该 target 的服务拉进来（反向依赖）
        if unit.is_target {
            for (other_name, other) in units.iter() {
                if other.install.wanted_by.iter().any(|w| w == name) {
                    deps.push(other_name.clone());
                }
            }
        }
        for dep in &deps {
            visit(dep, units, order, visited)?;
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
                description: String::new(),
                name: String::new(),
                after: after.iter().map(|s| s.to_string()).collect(),
                requires: requires.iter().map(|s| s.to_string()).collect(),
            },
            service: ServiceSection {
                typ: "simple".to_string(),
                exec_start: None,
                exec_stop: None,
                exec_reload: None,
                restart: String::new(),
                restart_sec: 1,
                start_limit_burst: 5,
                start_limit_interval_sec: 10,
                timeout_start_sec: 10,
                pidfile: None,
                environment: Vec::new(),
                logfile: None,
                user: None,
                group: None,
                console: false,
            },
            install: InstallSection {
                wanted_by: wanted_by.iter().map(|s| s.to_string()).collect(),
            },
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
}
