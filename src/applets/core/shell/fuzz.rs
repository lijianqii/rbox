//! 随机化健壮性测试（免依赖 fuzz-lite）。
//!
//! 用确定性 LCG 生成大量随机/畸形输入，验证 tokenizer/parser/expander/
//! glob/fstab 解析不会 panic（属性：任意输入都返回或报错，不崩溃）。
//! 真正的覆盖率由 `make coverage`（cargo-llvm-cov）提供。

use super::expander::{eval_arith, expand_glob, expand_vars};
use super::parser::build_command_list;
use super::tokenizer::tokenize;
use crate::applets::fstab::parse_fstab;
use crate::applets::glob::glob_match;

/// 简单 LCG（可复现，无需 rand 依赖）。
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[(self.next() as usize) % xs.len()]
    }
}

/// 生成随机命令行（包含引号/操作符/展开/UTF-8/畸形组合）。
fn random_line(rng: &mut Rng) -> String {
    const FRAGMENTS: &[&str] = &[
        "echo", "cat", " ", "  ", "'", "\"", "|", "&&", "||", ";", "&", ">", ">>", "<", "2>",
        "2>&1", ">&2", "$(", ")", "$((1+", "${", "}", "$?", "$$", "$1", "$#", "$@", "\\", "*", "?",
        "[", "]", "a=b", "你好", "héllo", "\t", "#c", "1>", "<<EOF", "(", "-n", "0",
    ];
    let n = 1 + (rng.next() as usize) % 12;
    let mut line = String::new();
    for _ in 0..n {
        line.push_str(rng.pick(FRAGMENTS));
    }
    line
}

#[test]
fn fuzz_tokenize_parse_never_panics() {
    let mut rng = Rng(0x1234_5678_9abc_def0);
    for _ in 0..5000 {
        let line = random_line(&mut rng);
        let tokens = tokenize(&line);
        let _ = build_command_list(&tokens);
        // 变量/算术展开不 panic，输出为合法字符串
        let _ = expand_vars(&line, 0);
        let _ = eval_arith(&line);
        let _ = expand_glob(&line);
    }
}

#[test]
fn fuzz_glob_never_panics() {
    let mut rng = Rng(0xdead_beef_0bad_f00d);
    const CHARS: &[&str] = &["*", "?", "[", "]", "-", "a", "b", "\\", "文", "!", "^", "1"];
    for _ in 0..5000 {
        let pn = 1 + (rng.next() as usize) % 6;
        let tn = 1 + (rng.next() as usize) % 6;
        let pattern: String = (0..pn).map(|_| rng.pick(CHARS)).collect();
        let text: String = (0..tn).map(|_| rng.pick(CHARS)).collect();
        let _ = glob_match(&pattern, &text);
    }
}

#[test]
fn fuzz_fstab_never_panics() {
    let mut rng = Rng(0x0f0f_0f0f_1234_5678);
    const FRAGMENTS: &[&str] = &[
        "proc",
        "/proc",
        "proc",
        "defaults",
        "0",
        "0",
        "#",
        " ",
        "\t",
        "",
        "\n",
        "/dev/vda",
        "/",
        "ext4",
        "rw,noatime",
    ];
    for _ in 0..5000 {
        let n = (rng.next() as usize) % 10;
        let mut line = String::new();
        for _ in 0..n {
            line.push_str(rng.pick(FRAGMENTS));
            line.push(' ');
        }
        let entries = parse_fstab(&line);
        for e in entries {
            // 解析出的字段不应为空
            assert!(!e.device.is_empty());
            assert!(!e.mountpoint.is_empty());
        }
    }
}

#[test]
fn fuzz_arith_never_panics() {
    let mut rng = Rng(0xfeed_face_cafe_babe);
    const FRAGMENTS: &[&str] = &[
        "1",
        "0",
        "9",
        "+",
        "-",
        "*",
        "/",
        "%",
        "(",
        ")",
        " ",
        "x",
        "$x",
        "999999999999999999",
        "(((",
        ")))",
    ];
    for _ in 0..5000 {
        let n = (rng.next() as usize) % 10;
        let expr: String = (0..n).map(|_| rng.pick(FRAGMENTS)).collect();
        let _ = eval_arith(&expr);
    }
}
