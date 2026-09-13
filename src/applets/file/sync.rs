//! `sync` - 刷新文件系统缓冲。
//!
//! 用法：sync

use crate::applet::Applet;
use std::process::ExitCode;

pub struct Sync;
pub static SYNC: &Sync = &Sync;

impl Applet for Sync {
    fn name(&self) -> &'static str {
        "sync"
    }
    fn help(&self) -> &'static str {
        "sync - flush filesystem buffers"
    }
    fn run(&self, _args: &[String]) -> ExitCode {
        unsafe { libc::sync() };
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(SYNC.name(), "sync");
        assert!(SYNC.help().contains("flush"));
    }

    #[test]
    fn sync_succeeds() {
        assert_eq!(SYNC.run(&[]), ExitCode::SUCCESS);
    }
}
