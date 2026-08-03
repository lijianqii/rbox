//! `uname` - 打印系统信息。
//!
//! 支持 `-s`(kernel name) `-n`(nodename) `-r`(release) `-m`(machine)
//! `-a`(all)。默认仅 `-s`。
use crate::applet::Applet;
use std::ffi::CStr;
use std::process::ExitCode;

pub struct Uname;
pub static UNAME: &Uname = &Uname;

fn field(b: &[libc::c_char]) -> String {
    // utsname 字段以 NUL 结尾，可直接取指针
    unsafe { CStr::from_ptr(b.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

impl Applet for Uname {
    fn name(&self) -> &'static str {
        "uname"
    }
    fn help(&self) -> &'static str {
        "uname [-asnrvm] - print system info"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut raw: libc::utsname = unsafe { std::mem::zeroed() };
        // SAFETY: uname 写入 raw，指针有效。
        let rc = unsafe { libc::uname(&mut raw) };
        if rc != 0 {
            eprintln!("uname: failed");
            return ExitCode::FAILURE;
        }

        let sysname = field(&raw.sysname);
        let nodename = field(&raw.nodename);
        let release = field(&raw.release);
        let version = field(&raw.version);
        let machine = field(&raw.machine);

        // 解析选项
        let mut want_s = false;
        let mut want_n = false;
        let mut want_r = false;
        let mut want_v = false;
        let mut want_m = false;
        let mut want_a = false;
        let mut any = false;

        for a in args {
            if a.starts_with('-') && a.len() > 1 {
                for c in a[1..].chars() {
                    match c {
                        'a' => {
                            want_a = true;
                            any = true;
                        }
                        's' => {
                            want_s = true;
                            any = true;
                        }
                        'n' => {
                            want_n = true;
                            any = true;
                        }
                        'r' => {
                            want_r = true;
                            any = true;
                        }
                        'v' => {
                            want_v = true;
                            any = true;
                        }
                        'm' => {
                            want_m = true;
                            any = true;
                        }
                        _ => {}
                    }
                }
            }
        }

        if !any {
            want_s = true;
        }

        if want_a {
            want_s = true;
            want_n = true;
            want_r = true;
            want_v = true;
            want_m = true;
        }

        let mut parts: Vec<String> = Vec::new();
        if want_s {
            parts.push(sysname);
        }
        if want_n {
            parts.push(nodename);
        }
        if want_r {
            parts.push(release);
        }
        if want_v {
            parts.push(version);
        }
        if want_m {
            parts.push(machine);
        }

        println!("{}", parts.join(" "));
        ExitCode::SUCCESS
    }
}
