//! 全局配置：`/etc/rbox.conf`（TOML）。
//!
//! 所有字段可选，缺省使用代码内的默认值（与不配文件时的行为完全一致）。
//! 加载策略：进程内只解析一次（OnceLock），解析失败或文件缺失回退默认值。

use serde::Deserialize;
use std::sync::OnceLock;

/// 全局配置根。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct RboxConfig {
    #[serde(rename = "paths")]
    pub(crate) paths: PathsConfig,
    #[serde(rename = "getty")]
    pub(crate) getty: GettyConfig,
    #[serde(rename = "login")]
    pub(crate) login: LoginConfig,
    #[serde(rename = "init")]
    pub(crate) init: InitConfig,
    #[serde(rename = "shell")]
    pub(crate) shell: ShellConfig,
}

/// 路径类配置。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct PathsConfig {
    /// init 单元目录
    pub(crate) system_dir: String,
    /// 启动根 target
    pub(crate) default_target: String,
    /// init 控制协议 socket
    pub(crate) status_socket: String,
    /// 用户账号文件
    pub(crate) passwd: String,
    /// 影子密码文件
    pub(crate) shadow: String,
    /// 登录后欢迎信息
    pub(crate) motd: String,
    /// shell 启动时 source 的 profile
    pub(crate) profile: String,
    /// shell 历史文件（支持 `~` 前缀；为空则用 $HOME/.rbox_history）
    pub(crate) history_file: String,
    /// 挂载表
    pub(crate) fstab: String,
    /// 主机名文件
    pub(crate) hostname: String,
    /// sysctl 配置文件
    pub(crate) sysctl_conf: String,
    /// 内存信息文件（meminfo 命令读取）
    pub(crate) meminfo: String,
    /// 物理内存映射文件（meminfo 命令读取）
    pub(crate) iomem: String,
}

impl Default for PathsConfig {
    fn default() -> Self {
        Self {
            system_dir: "/etc/rbox/system".to_string(),
            default_target: "default.target".to_string(),
            status_socket: "/tmp/rbox.sock".to_string(),
            passwd: "/etc/passwd".to_string(),
            shadow: "/etc/shadow".to_string(),
            motd: "/etc/motd".to_string(),
            profile: "/etc/profile".to_string(),
            history_file: String::new(),
            fstab: "/etc/fstab".to_string(),
            hostname: "/etc/hostname".to_string(),
            sysctl_conf: "/etc/sysctl.conf".to_string(),
            meminfo: "/proc/meminfo".to_string(),
            iomem: "/proc/iomem".to_string(),
        }
    }
}

/// rgetty 配置。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct GettyConfig {
    /// rgetty exec 的登录程序
    pub(crate) login_program: String,
    /// 登录提示文本
    pub(crate) prompt: String,
    /// 未显式给 -t 时的默认超时秒数（None = 不超时）
    pub(crate) default_timeout: Option<u64>,
    /// 登录前横幅文件（不存在则跳过）
    pub(crate) issue_file: String,
    /// 登录失败（子进程非零退出）后重新提示前的延迟秒数
    pub(crate) failure_delay: u64,
}

impl Default for GettyConfig {
    fn default() -> Self {
        Self {
            login_program: "/bin/rlogin".to_string(),
            prompt: "rbox login: ".to_string(),
            default_timeout: None,
            issue_file: "/etc/issue".to_string(),
            failure_delay: 1,
        }
    }
}

/// rlogin 配置。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct LoginConfig {
    /// passwd 无 shell 字段时的缺省 shell
    pub(crate) shell: String,
    /// 密码提示文本
    pub(crate) password_prompt: String,
}

impl Default for LoginConfig {
    fn default() -> Self {
        Self {
            shell: "/bin/sh".to_string(),
            password_prompt: "Password: ".to_string(),
        }
    }
}

/// init 配置。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct InitConfig {
    /// 默认 PATH（init 启动时设置，shell/服务子进程继承）
    pub(crate) default_path: String,
}

impl Default for InitConfig {
    fn default() -> Self {
        Self {
            default_path: "/bin:/sbin:/usr/bin:/usr/sbin".to_string(),
        }
    }
}

/// shell 配置。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct ShellConfig {
    /// 未设置 $PS1 时的默认提示符
    pub(crate) default_ps1: String,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            default_ps1: "> ".to_string(),
        }
    }
}

/// 读取并缓存全局配置（进程内只解析一次）。
pub(crate) fn load() -> &'static RboxConfig {
    static CONFIG: OnceLock<RboxConfig> = OnceLock::new();
    CONFIG.get_or_init(|| {
        std::fs::read_to_string("/etc/rbox.conf")
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_hardcoded_values() {
        let cfg = RboxConfig::default();
        assert_eq!(cfg.paths.system_dir, "/etc/rbox/system");
        assert_eq!(cfg.paths.default_target, "default.target");
        assert_eq!(cfg.paths.status_socket, "/tmp/rbox.sock");
        assert_eq!(cfg.paths.passwd, "/etc/passwd");
        assert_eq!(cfg.paths.shadow, "/etc/shadow");
        assert_eq!(cfg.paths.motd, "/etc/motd");
        assert_eq!(cfg.paths.profile, "/etc/profile");
        assert_eq!(cfg.getty.login_program, "/bin/rlogin");
        assert_eq!(cfg.getty.prompt, "rbox login: ");
        assert_eq!(cfg.getty.issue_file, "/etc/issue");
        assert_eq!(cfg.getty.failure_delay, 1);
        assert_eq!(cfg.login.shell, "/bin/sh");
        assert_eq!(cfg.login.password_prompt, "Password: ");
        assert_eq!(cfg.init.default_path, "/bin:/sbin:/usr/bin:/usr/sbin");
        assert_eq!(cfg.paths.meminfo, "/proc/meminfo");
        assert_eq!(cfg.paths.iomem, "/proc/iomem");
        assert_eq!(cfg.shell.default_ps1, "> ");
    }

    #[test]
    fn parses_full_config() {
        let s = r#"
[paths]
system_dir = "/etc/rbox/system"
default_target = "multi-user.target"
status_socket = "/run/rbox.sock"
passwd = "/etc/passwd"
shadow = "/etc/shadow"
motd = "/etc/motd"
profile = "/etc/profile"
history_file = "~/.rbox_history"
fstab = "/etc/fstab"
hostname = "/etc/hostname"
sysctl_conf = "/etc/sysctl.conf"

[getty]
login_program = "/bin/rlogin"
prompt = "rbox login: "
default_timeout = 120
issue_file = "/etc/issue"
failure_delay = 2

[login]
shell = "/bin/ash"
password_prompt = "Password: "

[init]
default_path = "/bin:/sbin"
"#;
        let cfg: RboxConfig = toml::from_str(s).unwrap();
        assert_eq!(cfg.paths.default_target, "multi-user.target");
        assert_eq!(cfg.paths.status_socket, "/run/rbox.sock");
        assert_eq!(cfg.paths.history_file, "~/.rbox_history");
        assert_eq!(cfg.getty.default_timeout, Some(120));
        assert_eq!(cfg.getty.failure_delay, 2);
        assert_eq!(cfg.login.shell, "/bin/ash");
        assert_eq!(cfg.init.default_path, "/bin:/sbin");
    }

    #[test]
    fn partial_config_keeps_defaults() {
        // 只覆盖部分字段，其余保持默认
        let s = "[getty]\nprompt = \"test# \"\n";
        let cfg: RboxConfig = toml::from_str(s).unwrap();
        assert_eq!(cfg.getty.prompt, "test# ");
        assert_eq!(cfg.getty.login_program, "/bin/rlogin");
        assert_eq!(cfg.paths.system_dir, "/etc/rbox/system");
        assert_eq!(cfg.login.shell, "/bin/sh");
    }

    #[test]
    fn bad_config_falls_back_to_defaults() {
        assert!(toml::from_str::<RboxConfig>("not toml at all {{").is_err());
    }
}
