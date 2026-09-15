//! 定时器（`*.timer`）与路径监视（`*.path`）单元运行时。
//!
//! 定时器支持 OnBootSec/OnActiveSec（首次）、OnUnitActiveSec（重复）与简化
//! OnCalendar（`*:0/N` 每 N 分钟、`HH:MM[:SS]` 每天）。路径监视按 1s 粒度轮询
//! PathExists/PathChanged/DirectoryNotEmpty；命中后启动关联服务单元。

use crate::applets::core::init::units::{Unit, parse_duration};
use std::os::unix::io::AsRawFd;

/// 定时器实例。
pub(crate) struct TimerInstance {
    pub(crate) name: String,
    /// 触发的服务单元名
    pub(crate) target: String,
    /// 下次触发时刻
    pub(crate) next_due: std::time::Instant,
    /// 重复间隔（OnUnitActiveSec）
    pub(crate) repeat_secs: Option<u64>,
}

/// 由 timer 单元构造实例；无有效触发条件返回 None。
pub(crate) fn timer_from_unit(unit: &Unit) -> Option<TimerInstance> {
    if !unit.is_timer {
        return None;
    }
    let target = unit
        .timer
        .unit
        .clone()
        .unwrap_or_else(|| strip_suffix(&unit.name, ".timer"));
    let first = unit
        .timer
        .on_boot_sec
        .as_deref()
        .or(unit.timer.on_active_sec.as_deref())
        .and_then(parse_duration)
        .or_else(|| {
            unit.timer
                .on_calendar
                .as_deref()
                .and_then(calendar_delay_secs)
        })?;
    Some(TimerInstance {
        name: unit.name.clone(),
        target,
        next_due: std::time::Instant::now() + std::time::Duration::from_secs(first),
        repeat_secs: unit
            .timer
            .on_unit_active_sec
            .as_deref()
            .and_then(parse_duration),
    })
}

/// 简化 OnCalendar：返回距下次触发的秒数。
/// `*:0/N` → 每 N 分钟对齐到整点后的 N 分钟倍数；`HH:MM[:SS]` → 每天该时刻。
pub(crate) fn calendar_delay_secs(spec: &str) -> Option<u64> {
    let spec = spec.trim();
    if let Some(rest) = spec.strip_prefix("*:0/") {
        let n: u64 = rest.trim().parse().ok()?;
        if n == 0 {
            return None;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        let secs = now.as_secs();
        let interval = n * 60;
        let next = ((secs / interval) + 1) * interval;
        return Some(next - secs);
    }
    let parts: Vec<&str> = spec.split(':').collect();
    if parts.len() < 2 {
        return None;
    }
    let hour: u64 = parts[0].trim().parse().ok()?;
    let minute: u64 = parts[1].trim().parse().ok()?;
    let second: u64 = parts
        .get(2)
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    let secs = now.as_secs();
    let day = secs % 86400;
    let target = hour * 3600 + minute * 60 + second;
    Some(if target > day {
        target - day
    } else {
        86400 - day + target
    })
}

/// 路径监视类型。
pub(crate) enum PathKind {
    Exists,
    Changed,
    DirNotEmpty,
}

/// 路径监视实例。
pub(crate) struct PathInstance {
    pub(crate) name: String,
    pub(crate) target: String,
    pub(crate) kind: PathKind,
    pub(crate) path: String,
    /// 上次观察到的 (mtime 秒, 大小)；PathChanged 用
    last: Option<(u64, u64)>,
    /// PathExists/DirNotEmpty 是否已触发过（只触发一次）
    fired: bool,
}

impl PathInstance {
    /// 检查路径状态；返回 true 表示需要触发关联单元。
    pub(crate) fn check(&mut self) -> bool {
        match self.kind {
            PathKind::Exists => {
                if !self.fired && std::path::Path::new(&self.path).exists() {
                    self.fired = true;
                    return true;
                }
                false
            }
            PathKind::DirNotEmpty => {
                if !self.fired
                    && std::fs::read_dir(&self.path)
                        .map(|mut it| it.next().is_some())
                        .unwrap_or(false)
                {
                    self.fired = true;
                    return true;
                }
                false
            }
            PathKind::Changed => {
                let meta = std::fs::metadata(&self.path).ok();
                let cur = meta.map(|m| {
                    use std::os::unix::fs::MetadataExt;
                    (m.mtime() as u64, m.len())
                });
                let changed = match (&self.last, &cur) {
                    (Some(prev), Some(now)) => prev != now,
                    (None, Some(_)) => true,
                    _ => false,
                };
                self.last = cur;
                changed
            }
        }
    }
}

/// 由 path 单元构造监视实例（PathExists 优先于 PathChanged/DirectoryNotEmpty）。
pub(crate) fn path_from_unit(unit: &Unit) -> Option<PathInstance> {
    if !unit.is_path {
        return None;
    }
    let target = unit
        .path
        .unit
        .clone()
        .unwrap_or_else(|| strip_suffix(&unit.name, ".path"));
    let (kind, path) = if let Some(p) = unit.path.path_exists.first() {
        (PathKind::Exists, p.clone())
    } else if let Some(p) = unit.path.path_changed.first() {
        (PathKind::Changed, p.clone())
    } else {
        let p = unit.path.directory_not_empty.first()?;
        (PathKind::DirNotEmpty, p.clone())
    };
    Some(PathInstance {
        name: unit.name.clone(),
        target,
        kind,
        path,
        last: None,
        fired: false,
    })
}

fn strip_suffix(name: &str, suffix: &str) -> String {
    name.strip_suffix(suffix).unwrap_or(name).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_parsing() {
        assert_eq!(parse_duration("5"), Some(5));
        assert_eq!(parse_duration("5s"), Some(5));
        assert_eq!(parse_duration("2min"), Some(120));
        assert_eq!(parse_duration("1h"), Some(3600));
        assert_eq!(parse_duration("1d"), Some(86400));
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("abc"), None);
    }

    #[test]
    fn calendar_every_n_minutes() {
        let d = calendar_delay_secs("*:0/5").unwrap();
        assert!(d > 0 && d <= 300, "delay={}", d);
        assert_eq!(calendar_delay_secs("*:0/0"), None);
        assert_eq!(calendar_delay_secs("bad"), None);
    }

    #[test]
    fn calendar_daily() {
        let d = calendar_delay_secs("23:59:59").unwrap();
        assert!(d > 0 && d <= 86400);
        assert_eq!(calendar_delay_secs("25:00"), None);
    }

    #[test]
    fn timer_from_unit_requires_trigger() {
        let mut u = Unit {
            name: "job.timer".to_string(),
            is_timer: true,
            ..Default::default()
        };
        assert!(timer_from_unit(&u).is_none());
        u.timer.on_boot_sec = Some("5s".to_string());
        let t = timer_from_unit(&u).unwrap();
        assert_eq!(t.target, "job");
        assert!(t.repeat_secs.is_none());
        u.timer.on_unit_active_sec = Some("1min".to_string());
        assert_eq!(timer_from_unit(&u).unwrap().repeat_secs, Some(60));
    }

    #[test]
    fn path_exists_triggers_once() {
        let path = format!("/tmp/rbox_path_test_{}", std::process::id());
        let _ = std::fs::remove_file(&path);
        let mut p = PathInstance {
            name: "p".to_string(),
            target: "svc".to_string(),
            kind: PathKind::Exists,
            path: path.clone(),
            last: None,
            fired: false,
        };
        assert!(!p.check());
        std::fs::write(&path, "x").unwrap();
        assert!(p.check());
        assert!(!p.check()); // 只触发一次
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn path_changed_detects_mtime() {
        let path = format!("/tmp/rbox_path_chg_{}", std::process::id());
        std::fs::write(&path, "a").unwrap();
        let mut p = PathInstance {
            name: "p".to_string(),
            target: "svc".to_string(),
            kind: PathKind::Changed,
            path: path.clone(),
            last: None,
            fired: false,
        };
        assert!(p.check()); // 首次建立基线即视为变化
        assert!(!p.check()); // 无变化
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&path, "bb").unwrap();
        assert!(p.check()); // 大小/时间变化
        let _ = std::fs::remove_file(&path);
    }
}

/// socket 激活监听器。
pub(crate) enum Listener {
    Unix(std::os::unix::net::UnixListener),
    Tcp(std::net::TcpListener),
}

/// socket 单元实例。
pub(crate) struct SocketInstance {
    pub(crate) name: String,
    pub(crate) target: String,
    /// true：每个连接启动一次服务（连接作为 stdin/stdout）
    pub(crate) accept: bool,
    pub(crate) listener: Listener,
}

impl SocketInstance {
    pub(crate) fn fd(&self) -> i32 {
        match &self.listener {
            Listener::Unix(l) => l.as_raw_fd(),
            Listener::Tcp(l) => l.as_raw_fd(),
        }
    }

    /// 接受一个连接（Unix/TCP 均返回已连接的流）。
    pub(crate) fn accept_conn(&self) -> Option<SocketConn> {
        match &self.listener {
            Listener::Unix(l) => {
                let (s, _) = l.accept().ok()?;
                Some(SocketConn::Unix(s))
            }
            Listener::Tcp(l) => {
                let (s, _) = l.accept().ok()?;
                Some(SocketConn::Tcp(s))
            }
        }
    }
}

/// 已接受的连接。
pub(crate) enum SocketConn {
    Unix(std::os::unix::net::UnixStream),
    Tcp(std::net::TcpStream),
}

/// 由 socket 单元创建监听器（ListenStream 为 `/path` 或端口号）。
pub(crate) fn socket_from_unit(unit: &Unit) -> Option<SocketInstance> {
    if !unit.is_socket {
        return None;
    }
    let target = unit
        .socket
        .unit
        .clone()
        .unwrap_or_else(|| strip_suffix(&unit.name, ".socket"));
    let spec = unit.socket.listen_stream.as_deref()?;
    let listener = if spec.starts_with('/') {
        let _ = std::fs::remove_file(spec); // 清理残留套接字文件
        let l = std::os::unix::net::UnixListener::bind(spec).ok()?;
        if let Some(mode) = unit.socket.socket_mode {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(spec, std::fs::Permissions::from_mode(mode));
        }
        Listener::Unix(l)
    } else {
        let port: u16 = spec.trim().parse().ok()?;
        Listener::Tcp(std::net::TcpListener::bind(("127.0.0.1", port)).ok()?)
    };
    Some(SocketInstance {
        name: unit.name.clone(),
        target,
        accept: unit.socket.accept,
        listener,
    })
}

#[cfg(test)]
mod socket_tests {
    use super::*;

    #[test]
    fn socket_from_unit_unix() {
        let path = format!("/tmp/rbox_sock_test_{}.sock", std::process::id());
        let _ = std::fs::remove_file(&path);
        let u = Unit {
            name: "echo.socket".to_string(),
            is_socket: true,
            socket: crate::applets::core::init::units::SocketSection {
                listen_stream: Some(path.clone()),
                socket_mode: Some(0o666),
                ..Default::default()
            },
            ..Default::default()
        };
        let s = socket_from_unit(&u).unwrap();
        assert_eq!(s.target, "echo");
        assert!(!s.accept);
        assert!(std::path::Path::new(&path).exists());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn socket_from_unit_requires_listen() {
        let u = Unit {
            name: "x.socket".to_string(),
            is_socket: true,
            ..Default::default()
        };
        assert!(socket_from_unit(&u).is_none());
    }
}
