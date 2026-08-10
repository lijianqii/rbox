//! `false` - 永远失败退出。
use crate::applet::Applet;
use std::process::ExitCode;

pub struct False;
pub static FALSE: &False = &False;

impl Applet for False {
    fn name(&self) -> &'static str {
        "false"
    }
    fn help(&self) -> &'static str {
        "false - return failure status"
    }
    fn run(&self, _args: &[String]) -> ExitCode {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_help() {
        assert_eq!(FALSE.name(), "false");
        assert!(FALSE.help().contains("failure"));
    }
}
