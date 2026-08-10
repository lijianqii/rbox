//! `true` - 永远成功退出。
use crate::applet::Applet;
use std::process::ExitCode;

pub struct True;
pub static TRUE: &True = &True;

impl Applet for True {
    fn name(&self) -> &'static str {
        "true"
    }
    fn help(&self) -> &'static str {
        "true - return success status"
    }
    fn run(&self, _args: &[String]) -> ExitCode {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(TRUE.name(), "true");
        assert!(TRUE.help().contains("success"));
    }
}
