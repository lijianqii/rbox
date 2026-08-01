//! `echo` - 输出参数。
use crate::applet::Applet;
use std::io::Write;
use std::process::ExitCode;

pub struct Echo;
pub static ECHO: &Echo = &Echo;

impl Applet for Echo {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn help(&self) -> &'static str {
        "echo [-n] [args...] - print arguments"
    }
    fn run(&self, args: &[String]) -> ExitCode {
        let mut args = args.iter();
        let mut newline = true;
        let mut printed_any = false;

        // 仅识别首个 -n，之后全部当普通文本。
        if let Some(first) = args.next() {
            if first == "-n" {
                newline = false;
            } else {
                print!("{}", first);
                printed_any = true;
            }
        }

        for a in args {
            if printed_any {
                print!(" ");
            }
            print!("{}", a);
            printed_any = true;
        }

        if newline {
            println!();
        }

        match std::io::stdout().flush() {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        }
    }
}
