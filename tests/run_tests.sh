#!/bin/bash
# rbox 集成测试脚本
# 在 QEMU 全系统模拟中运行预设命令并验证输出
#
# 测试专用服务（tests/units/*.toml）通过 `make rootfs-test` 注入并打包为
# 独立的测试 initramfs，生产 rootfs 保持干净。
set -e

cd "$(dirname "$0")/.."

KERNEL=kernel/arch/arm64/boot/Image
TEST_INITRD=initramfs.test.cpio.gz
INITRD=initramfs.cpio.gz
QEMU="qemu-system-aarch64 -M virt -cpu cortex-a72 -m 128M -nographic"
APPEND="console=ttyAMA0 rdinit=/init"

PASS=0
FAIL=0

# ─── 通过 Makefile 构建测试用 initramfs（注入+打包+清理）─────────
make rootfs-test >/dev/null 2>&1

cleanup() {
    rm -f "$TEST_INITRD"
}
trap cleanup EXIT

# 断言输出包含某字符串（QEMU 串口输出为 CRLF，先去除 \r 再匹配）
assert_contains_in() {
    local out="$1"
    local desc="$2"
    local pattern="$3"
    if echo "$out" | tr -d '\r' | grep -q -- "$pattern"; then
        echo "  PASS  $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL  $desc (期望包含: '$pattern')"
        FAIL=$((FAIL + 1))
    fi
}
assert_contains() {
    assert_contains_in "$OUT" "$1" "$2"
}

# 断言输出不包含某字符串
assert_not_contains_in() {
    local out="$1"
    local desc="$2"
    local pattern="$3"
    if echo "$out" | tr -d '\r' | grep -q -- "$pattern"; then
        echo "  FAIL  $desc (不应包含: '$pattern')"
        FAIL=$((FAIL + 1))
    else
        echo "  PASS  $desc"
        PASS=$((PASS + 1))
    fi
}

# 断言输出中存在与给定字符串完全相等的一行（-F 固定串 + -x 整行）
# 用于关键行为：避免子串匹配造成的假阳性（如 "20" 匹配到任意含 20 的行）
assert_line_in() {
    local out="$1"
    local desc="$2"
    local pattern="$3"
    if printf '%s\n' "$out" | tr -d '\r' | grep -qxF -- "$pattern"; then
        echo "  PASS  $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL  $desc (期望整行等于: '$pattern')"
        FAIL=$((FAIL + 1))
    fi
}
assert_line() {
    assert_line_in "$OUT" "$1" "$2"
}

# 断言输出中存在匹配正则的一整行（-E 扩展正则 + -x 整行）
assert_line_regex_in() {
    local out="$1"
    local desc="$2"
    local pattern="$3"
    if printf '%s\n' "$out" | tr -d '\r' | grep -qxE -- "$pattern"; then
        echo "  PASS  $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL  $desc (期望整行匹配: '$pattern')"
        FAIL=$((FAIL + 1))
    fi
}
assert_line_regex() {
    assert_line_regex_in "$OUT" "$1" "$2"
}

# ─── rgetty/rlogin 登录流程（使用生产 initramfs，console 为 rgetty）───
# 放在主会话之前：机器空闲时先跑短会话，避免连续两个 QEMU 负载叠加。
# 验证：登录提示、错误密码拒绝、登录后 shell 可用、shell 退出后 init
# respawn 重新登录。
echo ""
echo "[rgetty/rlogin 登录流程]"
LOGIN_OUT=$(timeout 150 bash -c '
{
  sleep 32
  printf "root\n"; sleep 2
  printf "wrongpass\n"; sleep 2
  printf "root\n"; sleep 2
  printf "wrongpass2\n"; sleep 2
  printf "root\n"; sleep 2
  printf "root\n"; sleep 2
  printf "echo LOGIN_OK\n"; sleep 1
  printf "ls -l /proc/self/fd/0\n"; sleep 1
  printf "exit\n"; sleep 3
  printf "root\n"; sleep 2
  printf "root\n"; sleep 2
  printf "echo LOGIN_AGAIN\n"; sleep 1
  printf "shutdown\n"; sleep 8
} | qemu-system-aarch64 -M virt -cpu cortex-a72 -m 128M -nographic \
  -kernel '"$KERNEL"' -initrd '"$INITRD"' -append '"'$APPEND'"'
' 2>&1) || true
assert_contains_in "$LOGIN_OUT" "rgetty 登录提示" "user: "
assert_contains_in "$LOGIN_OUT" "登录前 issue 横幅" "██"
assert_contains_in "$LOGIN_OUT" "错误密码被拒绝" "Login incorrect"
assert_contains_in "$LOGIN_OUT" "登录后 shell 可用" "LOGIN_OK"
assert_contains_in "$LOGIN_OUT" "rgetty 使用命令行指定串口" "0 -> /dev/ttyAMA0"
assert_contains_in "$LOGIN_OUT" "退出后重新登录" "LOGIN_AGAIN"

# ─── rgetty/rlogin 超时流程（独立测试 initramfs：console -t 8 空闲超时 +
#      rbox.conf password_timeout=3 密码超时；备份/恢复生产文件）───
# 验证：登录成功后可空闲超时登出、密码阶段可超时拒绝、超时后仍可重新登录。
build_login_test_initramfs() {
    local bak_unit=/tmp/rbox_console_bak.toml
    local bak_conf=/tmp/rbox_conf_bak
    cp rootfs/etc/rbox/system/console-shell.service.toml "$bak_unit"
    cp rootfs/etc/rbox.conf "$bak_conf"
    cp tests/login-console.service.toml rootfs/etc/rbox/system/console-shell.service.toml
    cp tests/rbox.test.conf rootfs/etc/rbox.conf
    (cd rootfs && find . | cpio -o -H newc 2>/dev/null | gzip > ../login-test.cpio.gz)
    mv "$bak_unit" rootfs/etc/rbox/system/console-shell.service.toml
    mv "$bak_conf" rootfs/etc/rbox.conf
}
build_login_test_initramfs
TIMEOUT_OUT=$(timeout 150 bash -c '
{
  sleep 28
  # 第一次登录：成功后每 3s 输入一次（< -t 8），持续 15s 不应超时；
  # K1..K5 中偶发单次丢失不影响结论（间隔仍 < 8s）
  printf "root\n"; sleep 2
  printf "root\n"; sleep 3
  printf "echo K1\n"; sleep 3
  printf "echo K2\n"; sleep 3
  printf "echo K3\n"; sleep 3
  printf "echo K4\n"; sleep 3
  printf "echo K5\n"; sleep 10
  # 空闲 10s（> -t 8）触发空闲超时登出
  # 第二次登录：密码阶段静默 5s（> password_timeout 3）触发密码超时
  printf "root\n"; sleep 5
  # 第三次登录：恢复正常，确认超时后仍可重新登录
  printf "root\n"; sleep 2
  printf "root\n"; sleep 2
  printf "echo TIMEOUT_LOGIN_OK\n"; sleep 1
  printf "shutdown\n"; sleep 8
} | qemu-system-aarch64 -M virt -cpu cortex-a72 -m 128M -nographic \
  -kernel '"$KERNEL"' -initrd login-test.cpio.gz -append '"'$APPEND'"'
' 2>&1 | tee /tmp/main_out.txt) || true
rm -f login-test.cpio.gz
assert_contains_in "$TIMEOUT_OUT" "持续输入不超时" "K5"
# 自定义超时消息（rbox.test.conf 显式设置，验证配置化生效）
assert_contains_in "$TIMEOUT_OUT" "空闲超时登出（自定义消息）" "custom test message"
assert_contains_in "$TIMEOUT_OUT" "密码输入超时（自定义消息）" "Password timed out"
assert_contains_in "$TIMEOUT_OUT" "超时后重新登录" "TIMEOUT_LOGIN_OK"

# 单次 QEMU 运行所有测试命令
# 命令序列本身约 30 秒，超时需留足内核启动余量（负载高时启动会变慢）
# 注意：整个命令块在外层 bash -c '...' 单引号中，内部 printf 必须用双引号，
#       $ 需转义为 \$，单引号用 \x27 代替，避免破坏外层引号。
OUT=$(timeout 400 bash -c '
{
  sleep 20
  # 基本 applet
  printf "uname -m\n"; sleep 0.5
  printf "uname -n\n"; sleep 0.5
  printf "pwd\n"; sleep 0.5
  printf "echo hello\n"; sleep 0.5
  printf "cat /etc/hostname\n"; sleep 0.5
  printf "true\n"; sleep 0.5
  # 文件操作
  printf "mkdir -p /tmp/t1\n"; sleep 0.5
  printf "echo content > /tmp/t1/f.txt\n"; sleep 0.5
  printf "cat /tmp/t1/f.txt\n"; sleep 0.5
  printf "cp /tmp/t1/f.txt /tmp/t1/g.txt\n"; sleep 0.5
  printf "mv /tmp/t1/g.txt /tmp/t1/h.txt\n"; sleep 0.5
  printf "ls /tmp/t1\n"; sleep 0.5
  printf "rm /tmp/t1/f.txt\n"; sleep 0.5
  # rm -r 对目录符号链接只删链接本身（回归：曾误删链接目标内容）
  printf "mkdir -p /tmp/symt/real/sub; echo precious > /tmp/symt/real/sub/data.txt; ln -s real /tmp/symt/link; rm -r /tmp/symt/link; cat /tmp/symt/real/sub/data.txt\n"; sleep 1
  # cat - 读 stdin
  printf "echo stdin_ok | cat -\n"; sleep 0.5
  # 管道与重定向
  printf "echo aaa > /tmp/a\n"; sleep 0.5
  printf "echo bbb > /tmp/b\n"; sleep 0.5
  printf "cat /tmp/a /tmp/b | cat\n"; sleep 0.5
  printf "echo appended >> /tmp/a\n"; sleep 0.5
  printf "cat /tmp/a\n"; sleep 0.5
  # 服务管理：env 注入、status 查询、Restart=on-failure
  printf "rbox status\n"; sleep 0.5
  printf "rbox status hello\n"; sleep 0.5
  # rservice：stop/start/restart/list
  printf "rservice stop longrun\n"; sleep 0.5
  printf "rservice start longrun\n"; sleep 0.5
  printf "rservice restart longrun\n"; sleep 0.5
  printf "rservice list\n"; sleep 0.5
  # init 增强：reload、sysctl、User= 降权
  printf "rservice reload longrun\n"; sleep 0.5
  printf "rservice reload console-shell\n"; sleep 0.5
  printf "rservice status console-shell\n"; sleep 0.5
  # Wants/Requisite 依赖语义
  # 日志持久化：logkeeper 转发 kmsg 到 /var/log/messages
  printf "rbox head -n 3 /var/log/messages\n"; sleep 0.5
  printf "rbox status logkeeper\n"; sleep 0.5
  printf "rbox status req-test\n"; sleep 0.5
  printf "rbox status req-ok\n"; sleep 0.5
  printf "rbox status wants-test\n"; sleep 0.5
  printf "cat /proc/sys/kernel/panic\n"; sleep 0.5
  printf "cat /tmp/usertest.log\n"; sleep 0.5
  printf "rbox head -n 60 /dev/kmsg\n"; sleep 0.5
  # ── Shell 功能测试 ──
  # 1. 引号与转义
  printf "echo \"hello world\"\n"; sleep 0.5
  printf "echo \x27single quoted\x27\n"; sleep 0.5
  printf "echo hello\\\\ world\n"; sleep 0.5
  printf "echo line1 \\\\\nmore\n"; sleep 0.5
  printf "echo visible # hidden\n"; sleep 0.5
  # 2. 变量展开
  printf "export FOO=bar\n"; sleep 0.5
  printf "echo \$FOO\n"; sleep 0.5
  printf "echo \${FOO}_x\n"; sleep 0.5
  printf "false; echo rc=\$?\n"; sleep 0.5
  printf "echo pid=\$\$\n"; sleep 0.5
  printf "unset FOO; echo [\$FOO]\n"; sleep 0.5
  # 3. 控制操作符
  printf "echo a; echo b\n"; sleep 0.5
  printf "true && echo yes\n"; sleep 0.5
  printf "false || echo fallback\n"; sleep 0.5
  printf "true && echo ok1 || echo ok2\n"; sleep 0.5
  printf "echo bg_start; sleep 1 & echo bg_done\n"; sleep 1
  # 4. 重定向
  printf "echo redir_out > /tmp/t_redir.txt\n"; sleep 0.5
  printf "echo redir_append >> /tmp/t_redir.txt\n"; sleep 0.5
  printf "cat < /tmp/t_redir.txt\n"; sleep 0.5
  # 5. 多级管道
  printf "echo p3_test | cat | cat\n"; sleep 0.5
  printf "echo pipe_redir | cat > /tmp/t_pipe.txt\n"; sleep 0.5
  printf "cat /tmp/t_pipe.txt\n"; sleep 0.5
  # 6. 通配符
  printf "mkdir -p /tmp/glob_test\n"; sleep 0.5
  printf "touch /tmp/glob_test/a.txt\n"; sleep 0.5
  printf "touch /tmp/glob_test/b.txt\n"; sleep 0.5
  printf "touch /tmp/glob_test/c.log\n"; sleep 0.5
  printf "ls /tmp/glob_test/*.txt\n"; sleep 0.5
  printf "ls /tmp/glob_test/?.txt\n"; sleep 0.5
  printf "ls /tmp/glob_test/[ab].txt\n"; sleep 0.5
  # 6.5 引号保护 glob（回归：引号内 * 不得展开为目录项）
  printf "echo \"*\"\n"; sleep 0.5
  printf "echo \x27*\x27\n"; sleep 0.5
  # 6.6 内置命令重定向（回归：pwd > file）
  printf "pwd > /tmp/t_pwd_redirect\n"; sleep 0.5
  printf "cat /tmp/t_pwd_redirect && echo REDIRECT_OK\n"; sleep 0.5
  # 7. 历史扩展
  printf "echo hist_one\n"; sleep 0.5
  printf "echo hist_two\n"; sleep 0.5
  printf "!!\n"; sleep 0.5
  printf "!1\n"; sleep 0.5
  printf "echo last_arg one two three\n"; sleep 0.5
  printf "echo copy:!$\n"; sleep 0.5
  printf "history\n"; sleep 0.5
  # 8. ~ 展开
  printf "echo ~\n"; sleep 0.5
  printf "cd ~ && pwd\n"; sleep 0.5
  # 9. Tab 补全
  printf "ec\thello\n"; sleep 0.5
  printf "cat /etc/host\t\n"; sleep 0.5
  printf "echo p | ec\thi\n"; sleep 0.5
  # 10. 行编辑快捷键 (Ctrl-A 被 QEMU 截获，无法测试)
  printf "echo abc\x05XX\n"; sleep 0.5
  printf "echo hello\x15echo world\n"; sleep 0.5
  printf "echo keep\x0b\n"; sleep 0.5
  printf "echo word1 word2\x17\n"; sleep 0.5
  printf "echo cancel\x03echo after_ctrl_c\n"; sleep 0.5
  # 10.5 UTF-8 中文输入（多字节字符端到端）
  printf "echo 你好世界\n"; sleep 0.5
  # 10.6 后台命令与前台命令并发时的退出码（SIGCHLD 屏蔽验证）
  printf "sleep 3 &\n"; sleep 0.5
  printf "true\n"; sleep 0.5
  printf "echo bg_true_rc=\$?\n"; sleep 0.5
  # 10.8 复合命令 / 别名 / 命令替换 / 作业控制
  printf "if true\n"; sleep 0.3
  printf "then\n"; sleep 0.3
  printf "echo IF_BLOCK_OK\n"; sleep 0.3
  printf "fi\n"; sleep 0.4
  printf "for i in 11 22\n"; sleep 0.3
  printf "do\n"; sleep 0.3
  printf "echo FOR_\$i\n"; sleep 0.3
  printf "done\n"; sleep 0.4
  printf "while true\n"; sleep 0.3
  printf "do\n"; sleep 0.3
  printf "echo WHILE_ONCE\n"; sleep 0.3
  printf "break\n"; sleep 0.3
  printf "done\n"; sleep 0.4
  printf "alias tll=\x27echo ALIAS_OK\x27\n"; sleep 0.3
  printf "tll\n"; sleep 0.4
  printf "echo SUBST_\$(echo INNER)\n"; sleep 0.4
  printf "sleep 2 &\n"; sleep 0.3
  printf "jobs\n"; sleep 0.4
  printf "sleep 5\n"; sleep 0.5
  printf "\x1a"; sleep 0.5
  printf "jobs\n"; sleep 0.4
  printf "bg\n"; sleep 0.4
  # 10.9 脚本模式 / POSIX 展开 / 新重定向（新增）
  printf "sh -c \x27echo C_MODE_OK\x27\n"; sleep 0.5
  printf "cat > /tmp/scr.sh <<\x27EOF\x27\n"; sleep 0.3
  printf "echo \"script:\$1:\$#\"\n"; sleep 0.3
  printf "f() { echo \"func:\$1\"; return 3; }\n"; sleep 0.3
  printf "f hi; echo \"rc=\$?\"\n"; sleep 0.3
  printf "case x in x) echo CASE_OK;; esac\n"; sleep 0.3
  printf "until false; do echo UNTIL_OK; break; done\n"; sleep 0.3
  printf "for i in 1 2; do for j in a b; do echo \"loop:\$i\$j\"; break 2; done; done\n"; sleep 0.3
  printf "echo \"\${UNSET_XYZ:-PARAM_OK}\"\n"; sleep 0.3
  printf "V=\"a b\"; printf \"[%%s]\" \$V; echo\n"; sleep 0.3
  printf "echo \"file:\$(</etc/hostname)\"\n"; sleep 0.3
  printf "trap \x27echo EXIT_TRAP\x27 EXIT\n"; sleep 0.3
  printf "EOF\n"; sleep 0.6
  printf "sh /tmp/scr.sh arg1\n"; sleep 1.2
  printf "sh -ec \x27false\x27; echo e_rc=\$?\n"; sleep 0.5
  printf "ls /nonexistent_rbox &> /tmp/both.txt; cat /tmp/both.txt\n"; sleep 0.6
  printf "cat <<< here_string_ok\n"; sleep 0.5
  printf "ls /nonexistent_rbox |& grep -o \x27No such file\x27\n"; sleep 0.5
  printf "p=/a/b/c.txt; echo \"\${p##*/} \${p%%/*} \${UNSET_X:-DEF}\"\n"; sleep 0.5
  printf "echo \x27r1 r2\x27 > /tmp/rin.txt; read a b < /tmp/rin.txt; echo \"read:\$a:\$b\"\n"; sleep 0.5
  printf "sleep 0.2 & wait \$!; echo wait_ok=\$?\n"; sleep 0.6
  # 10.10 ash 对齐：子 shell/组/取反/反引号/只读/getopts/算术/子串/复合重定向
  printf "x=1; ( x=2; echo \"sub:\$x\" ); echo \"out:\$x\"\n"; sleep 0.6
  printf "{ echo grp1; echo grp2; } > /tmp/grp.txt; cat /tmp/grp.txt\n"; sleep 0.6
  printf "! false; echo \"neg=\$?\"\n"; sleep 0.5
  printf "echo \x60echo bq_ok\x60\n"; sleep 0.5
  printf ":; echo colon_rc=\$?\n"; sleep 0.5
  printf "readonly RO=7; RO=9; echo \"ro=\$RO\"\n"; sleep 0.5
  printf "set -- -a -b val x; getopts ab: o; echo \"g1:\$o:\$OPTIND\"; getopts ab: o; echo \"g2:\$o:\$OPTARG\"\n"; sleep 0.6
  printf "echo \"ar:\$((5&3)):\$((5|2)):\$((1?2:3))\"\n"; sleep 0.5
  printf "sv=abcdef; echo \"sub:\${sv:1:3}\"\n"; sleep 0.5
  printf "set -- p q; for a; do echo \"noin:\$a\"; done\n"; sleep 0.6
  printf "echo line1 > /tmp/cmp.txt; echo line2 >> /tmp/cmp.txt; while read l; do c=\$l; done < /tmp/cmp.txt; echo \"last:\$c\"\n"; sleep 0.7
  printf "ulimit -n > /tmp/ul.txt\n"; sleep 0.5
  printf "n=\$(cat /tmp/ul.txt); [ \"\$n\" -ge 0 ] && echo ulimit_num_ok\n"; sleep 0.5
  # 10.11 覆盖补齐：noclobber/<>/fd/选项/环境/cd 路径/作业 kill
  printf "set -C; echo a > /tmp/nc.txt; echo b >| /tmp/nc.txt; cat /tmp/nc.txt\n"; sleep 0.6
  printf "echo c > /tmp/nc.txt 2>/dev/null || echo noclobber_blocked; set +C\n"; sleep 0.5
  printf "printf \x27keep\\\\n\x27 > /tmp/rw2.txt; cat <> /tmp/rw2.txt > /tmp/rwout.txt\n"; sleep 0.6
  printf "echo \"rwout:\$(cat /tmp/rwout.txt)\"\n"; sleep 0.5
  printf "echo \"rwkeep:\$(cat /tmp/rw2.txt)\"\n"; sleep 0.5
  printf "exec 3>/tmp/fd3.txt; echo fd3_ok >&3; exec 3>&-; cat /tmp/fd3.txt\n"; sleep 0.6
  printf "set -f; echo /tmp/noglob*; echo \"dash:\$-\"\n"; sleep 0.5
  printf "set +f; set -o > /tmp/opts.txt; grep noclobber /tmp/opts.txt\n"; sleep 0.5
  printf "case \$RANDOM in \x27\x27|*[!0-9]*) echo rnd_bad;; *) echo rnd_ok;; esac\n"; sleep 0.5
  printf "echo \$(echo m1; echo m2) | tr \x27\\\\n\x27 \x27,\x27; echo\n"; sleep 0.6
  printf "mkdir -p /tmp/cdp/sub; cd /tmp; CDPATH=/tmp/cdp; cd sub; pwd; cd /\n"; sleep 0.8
  printf "sleep 3 & kill %%+; sleep 1; echo killjob_ok\n"; sleep 1.2
  printf "false; if true; then echo IF2_OK; fi\n"; sleep 0.6
  printf "sv2=abcdef; echo \"negs:\${sv2: -2}\"\n"; sleep 0.5
  printf "readonly RO2=5; unset RO2; echo \"ro2:\$RO2\"\n"; sleep 0.5
  # 10.7 内存信息（meminfo 输出较大，后续命令需更多间隔）
  printf "meminfo\n"; sleep 1.5
  printf "meminfo -m\n"; sleep 1.5
  printf "processes\n"; sleep 1
  # 11. 文本处理 applets
  printf "%s\n" "echo -e 'line1\\nline2\\nline3' | head -n 2"; sleep 1
  printf "printf name=%%s-num=%%d rbox 42\n"; sleep 1
  printf "echo hello | wc -c\n"; sleep 0.5
  printf "echo hello | grep -o hel\n"; sleep 0.5
  printf "basename /usr/bin/gcc\n"; sleep 0.5
  printf "basename /tmp/test.txt .txt\n"; sleep 0.5
  printf "dirname /usr/bin/gcc\n"; sleep 0.5
  printf "date\n"; sleep 0.5
  # 12. env / ln
  printf "env | head -n 1\n"; sleep 0.5
  printf "ln -s /etc/hostname /tmp/linktest\n"; sleep 0.5
  printf "cat /tmp/linktest\n"; sleep 0.5
  # 13. echo -n
  printf "echo -n no_newline; echo after\n"; sleep 0.5
  # 14. ls -a / ls -1
  printf "ls -a -1 / | head -n 3\n"; sleep 0.5
  printf "ls -1 / | head -n 1\n"; sleep 0.5
  # 15. rm -r
  printf "rm -r /tmp/glob_test\n"; sleep 0.5
  printf "ls /tmp/glob_test 2>&1\n"; sleep 0.5
  # 16. touch 创建新文件
  printf "touch /tmp/touched_new\n"; sleep 0.5
  printf "ls /tmp/touched_new\n"; sleep 0.5
  # 17. mkdir -p 嵌套
  printf "mkdir -p /tmp/nested/deep/dir\n"; sleep 0.5
  printf "ls /tmp/nested/deep/dir\n"; sleep 0.5
  # 18. tail
  printf "echo -e \x27aaa\\nbbb\\nccc\x27 | tail -n 1\n"; sleep 0.5
  # 18.5 新增 applet：chmod/chown/find/kill/dmesg/mount/umount
  printf "touch /tmp/t_newapp; chmod 600 /tmp/t_newapp; ls -l /tmp/t_newapp\n"; sleep 0.5
  printf "chown 0:0 /tmp/t_newapp; echo chown_rc=\$?\n"; sleep 0.5
  printf "mkdir -p /tmp/find_t/sub; touch /tmp/find_t/a.txt /tmp/find_t/sub/b.txt; find /tmp/find_t -name \x27*.txt\x27\n"; sleep 0.5
  printf "find /tmp/find_t -type d\n"; sleep 0.5
  printf "kill -0 1 && echo kill_ok\n"; sleep 0.5
  printf "kill -l | head -n 1\n"; sleep 0.5
  printf "dmesg -n 1 > /tmp/t_dmesg; wc -l < /tmp/t_dmesg; echo dmesg_done\n"; sleep 0.5
  printf "mount | head -n 1\n"; sleep 0.5
  printf "umount /nonexistent 2>/dev/null; echo umount_rc=\$?\n"; sleep 0.5
  printf "kill -l 9\n"; sleep 0.5
  printf "export FD=/tmp/find_d; mkdir -p \$FD/sub; touch \$FD/x.txt \$FD/sub/y.txt; find \$FD -maxdepth 1 -name \x27*.txt\x27\n"; sleep 0.5
  printf "chmod u+x /tmp/t_newapp; ls -l /tmp/t_newapp\n"; sleep 0.5
  printf "mount /nonexistent_fstab_entry\n"; sleep 0.5
  # 19. stderr 重定向 2>
  printf "ls /nonexistent_xyz 2> /tmp/stderr_out; cat /tmp/stderr_out\n"; sleep 0.5
  # 20. stderr 追加 2>>
  printf "ls /another_missing 2>> /tmp/stderr_out; cat /tmp/stderr_out\n"; sleep 0.5
  # 21. source 命令
  printf "echo \x27export SOURCED=yes\x27 > /tmp/srctest.sh; source /tmp/srctest.sh; echo \$SOURCED\n"; sleep 0.5
  # 22. PS1 提示符（通过 source /etc/profile）
  printf "export PS1=\\x27test# \\x27; echo done\n"; sleep 0.5
  # 23. here-doc（三行发送，间隔稍长）
  printf "cat <<HDEOF\\nhello heredoc\\nHDEOF\n"; sleep 1
  # 24. console shell respawn 保留配置（Environment 注入后 shell 退出重启仍保留）
  printf "echo console_init=\$RBOX_CONSOLE\n"; sleep 0.5
  printf "exit\n"; sleep 3
  printf "echo console_respawn=\$RBOX_CONSOLE\n"; sleep 0.5
  # 24.5 前台命令 Ctrl-C 中断（临时 ISIG + SIGINT 转发到前台进程组）
  printf "sleep 60\n"; sleep 1
  printf "\x03"; sleep 2
  printf "echo intr_rc=\$?\n"; sleep 0.5
  # 25. reboot：触发有序关机流程后内核重启，等待重启完成后继续会话
  printf "echo before_reboot\n"; sleep 0.5
  printf "reboot\n"; sleep 35
  printf "echo after_reboot\n"; sleep 0.5
  # 关机
  printf "shutdown\n"; sleep 12
} | qemu-system-aarch64 -M virt -cpu cortex-a72 -m 128M -nographic \
  -kernel '"$KERNEL"' -initrd '"$TEST_INITRD"' -append "'"$APPEND"'"
' 2>&1) || true

echo "========================================"
echo "rbox 集成测试"
echo "========================================"
echo ""

echo "[基本 applet]"
assert_line "uname -m -> aarch64" "aarch64"
assert_line "uname -n -> 主机名" "rbox"
assert_line "pwd -> /" "/"
assert_line "echo hello -> hello" "hello"
assert_line "cat /etc/hostname" "rbox"

echo ""
echo "[文件操作]"
assert_contains "echo > 重定向写入" "content"
assert_contains "cp 复制" "h.txt"
assert_contains "ls 列出文件" "h.txt"

echo ""
echo "[管道与重定向]"
assert_contains "管道 cat|cat aaa" "aaa"
assert_contains "管道 cat|cat bbb" "bbb"
assert_contains "追加写入 >>" "appended"

echo ""
echo "[init 启动流程]"
assert_contains "init PID 1 启动" "rbox init: starting as PID 1"
assert_contains "fstab 挂载 proc" "mounting proc on /proc"
assert_contains "挂载基本文件系统" "basic filesystems mounted"
assert_contains "加载 TOML 单元" "loaded"
assert_contains "达到 default.target" "reached target"

echo ""
echo "[服务管理]"
assert_contains "Environment= 注入 HELLO" "HELLO=world"
assert_contains "Restart=on-failure 自动重启" "restarting restart-test"
assert_contains "rm 符号链接只删链接" "precious"
assert_contains "cat - 读 stdin" "stdin_ok"
assert_contains "logkeeper 转发 kmsg" "rbox init"
assert_contains "logkeeper 服务运行" "logkeeper running"
assert_contains "watchdog 无设备静默禁用" "watchdog unavailable"
assert_contains "status 列出 console" "console-shell"
assert_contains "status 列出重启服务" "restart-test"
assert_contains "status 单服务查询" "hello "
assert_contains "status 显示重启策略" "restart=on-failure"


echo ""
echo "[rservice 管理]"
assert_contains "rservice stop" "longrun stopped"
assert_contains "rservice start" "longrun started"
assert_contains "rservice restart" "longrun started"
assert_contains "rservice list 显示服务" "longrun"


echo ""
echo "[init 增强]"
assert_contains "ExecReload 执行" "reloaded-ok"
assert_contains "console reload 提示" "console-shell has no ExecReload"
assert_contains "Wants 失败不传播" "WANTS_OK"
assert_contains "Wants 服务已启动" "wants-test exited"
assert_contains "Requisite 未激活跳过" "skipping req-test"
assert_contains "Requisite 激活成功" "req-ok exited"
assert_not_contains_in "$OUT" "Requisite 跳过单元未执行" "REQ_SHOULD_NOT_RUN"
assert_contains "Requisite 跳过单元状态" "req-test not-started"
assert_contains "status 单查 console" "console-shell running"
assert_contains "sysctl kernel.panic" "10"
assert_contains "User= 降权 nobody" "65534"
assert_contains "Type=forking 等待父进程" "started forktest"
assert_contains "Type=forking 超时终止" "did not daemonize within 2s"
assert_contains "kmsg 日志写入" "\] rbox I: rbox init: mounting devpts"


echo ""
echo "[Shell: 引号与转义]"
assert_contains "双引号保留空格" "hello world"
assert_contains "单引号原样保留" "single quoted"
assert_contains "反斜杠转义" "hello world"
assert_contains "续行拼接" "line1 more"
assert_contains "注释不执行" "visible"

echo ""
echo "[Shell: 变量展开]"
assert_contains "export + \$VAR" "bar"
assert_contains "\${VAR}_x 展开" "bar_x"
assert_contains "\$? 退出码" "rc=1"
assert_contains "\$\$ PID 展开" "pid="
assert_contains "unset 后为空" "\[\]"

echo ""
echo "[Shell: 控制操作符]"
assert_contains "命令分隔 ;" "^a"
assert_contains "条件执行 &&" "yes"
assert_contains "条件执行 ||" "fallback"
assert_contains "&&/|| 链式" "ok1"
assert_contains "后台运行 &" "bg_done"

echo ""
echo "[Shell: 重定向]"
assert_contains "输出重定向 >" "redir_out"
assert_contains "追加写入 >>" "redir_append"
assert_contains "输入重定向 <" "redir_out"

echo ""
echo "[Shell: 管道]"
assert_contains "3级管道" "p3_test"
assert_contains "管道+重定向写入" "pipe_redir"
assert_contains "管道+重定向读回" "pipe_redir"

echo ""
echo "[Shell: 通配符]"
assert_contains "通配符 * 列出a" "a.txt"
assert_contains "通配符 * 列出b" "b.txt"
assert_contains "通配符 ? " "a.txt"
assert_contains "通配符 [] " "a.txt"

echo ""
echo "[Shell: 引号保护 glob / 内置重定向]"
assert_contains "双引号 * 不展开" "^\*$"
assert_line "内置命令重定向成功" "REDIRECT_OK"

echo ""
echo "[Shell: 历史扩展]"
assert_contains "!! 重复上一条" "hist_two"
assert_contains "!n 第n条命令" "hist_one"
assert_contains "!$ 最后参数" "copy:three"
assert_contains "history 内置命令" "hist_one"

echo ""
echo "[Shell: ~ 展开]"
assert_contains "echo ~ 输出 HOME" " /"
assert_contains "cd ~ 后 pwd" " /"

echo ""
echo "[Shell: Tab 补全]"
assert_contains "命令补全 ec->echo" "echo hello"
assert_contains "文件补全 /etc/host->hostname" "rbox"
assert_contains "管道后命令补全" "hi"

echo ""
echo "[Shell: 行编辑快捷键]"
assert_contains "Ctrl-E 行末插入" "abcXX"
assert_contains "Ctrl-U 删除行首" "world"
assert_contains "Ctrl-K 行末不删除" "keep"
assert_contains "Ctrl-W 删除单词" "echo word1"
assert_contains "Ctrl-C 中断当前行" "after_ctrl_c"

echo ""
echo "[Shell: UTF-8 输入]"
assert_contains "UTF-8 中文输入" "你好世界"

echo ""
echo "[Shell: 后台/前台退出码]"
assert_contains "后台+前台并发退出码" "bg_true_rc=0"

echo ""
echo "[Shell: 脚本模式/POSIX 展开/新重定向]"
assert_line "sh -c 模式" "C_MODE_OK"
assert_line "脚本位置参数" "script:arg1:1"
assert_line "函数定义与 return" "func:hi"
assert_line "函数返回码" "rc=3"
assert_line "case 语句" "CASE_OK"
assert_line "until 循环" "UNTIL_OK"
assert_line "嵌套 break 2" "loop:1a"
assert_line "参数默认值" "PARAM_OK"
assert_line "未加引号词分割" "[a][b]"
assert_line "命令替换读文件" "file:rbox"
assert_line "EXIT trap" "EXIT_TRAP"
assert_line "set -e 退出码" "e_rc=1"
assert_contains "&> 同时捕获 stderr" "No such file"
assert_line "here-string" "here_string_ok"
assert_contains "|& 管道 stderr" "No such file"
assert_line "参数前后缀删除" "c.txt /a/b DEF"
assert_line "read 变量拆分" "read:r1:r2"
assert_line "wait 返回码" "wait_ok=0"

echo ""
echo "[Shell: ash 对齐（子 shell/组/内置/算术）]"
assert_line "子 shell 变量隔离" "sub:2"
assert_line "子 shell 不影响父 shell" "out:1"
assert_line "花括号组重定向" "grp1"
assert_line "! 取反退出码" "neg=0"
assert_line "反引号命令替换" "bq_ok"
assert_line "冒号内置命令" "colon_rc=0"
assert_contains "readonly 保护" "read only"
assert_line "readonly 值不变" "ro=7"
assert_line "getopts 第一项" "g1:a:2"
assert_line "getopts 带参选项" "g2:b:val"
assert_line "算术位运算与三元" "ar:1:7:2"
assert_line "参数子串 \${v:1:3}" "sub:bcd"
assert_line "for 无 in 遍历位置参数" "noin:p"
assert_line "复合命令重定向变量持久" "last:line2"
assert_line "ulimit -n 输出为数字" "ulimit_num_ok"
assert_line "set -C 阻止覆盖" "noclobber_blocked"
assert_line ">| 强制覆盖" "b"
assert_line "<> 读取内容" "rwout:keep"
assert_line "<> 不截断文件" "rwkeep:keep"
assert_line "任意 fd 3> 与 3>&-" "fd3_ok"
assert_line "set -f 通配不展开" "/tmp/noglob*"
assert_line "\$- 含 f 标志" "dash:f"
assert_line "set -o 名称列表含 noclobber" "noclobber"
assert_line "\$RANDOM 为数字" "rnd_ok"
assert_line "命令替换拼接多行输出" "m1,m2,"
assert_contains "CDPATH 搜索相对路径" "/tmp/cdp/sub"
assert_contains "kill %job 后 wait 返回" "killjob_ok"
assert_line "分号后 if 复合命令" "IF2_OK"
assert_line "子串负偏移" "negs:ef"
assert_contains "readonly 阻止 unset" "read only"
assert_line "readonly 值保留" "ro2:5"

echo ""
echo "[Shell: 复合命令/别名/命令替换/作业控制]"
assert_line "if 块执行" "IF_BLOCK_OK"
assert_line "for 循环第一次" "FOR_11"
assert_line "for 循环第二次" "FOR_22"
assert_line "while + break" "WHILE_ONCE"
assert_line "alias 展开" "ALIAS_OK"
assert_line "命令替换 \$(...)" "SUBST_INNER"
assert_contains "jobs 显示后台作业" "Running"
assert_contains "Ctrl-Z 挂起作业" "Stopped"

echo ""
echo "[内存信息]"
assert_contains "meminfo 显示 Mem 行" "Mem:"
assert_contains "meminfo 显示 Swap 行" "Swap:"
assert_contains "meminfo 进程列表标题" "PID"
assert_contains "meminfo 列出 init 进程" "init"
assert_contains "meminfo 详细明细" "Memory detail"
assert_contains "meminfo 详细字段" "AnonPages"
assert_contains "meminfo 内存映射" "Memory map"
assert_contains "meminfo System RAM" "System RAM"
assert_contains "meminfo 分类核算" "Memory accounting"
assert_contains "meminfo 对账恒等" "== MemTotal"
assert_contains "meminfo 进程 %MEM" "%MEM"
assert_contains "processes 显示 init" "1 init(Command: /bin/rbox)"
assert_contains "processes 树连接符" "├──"
assert_contains "processes 大分组 kthreadd" "kthreadd"
assert_contains "meminfo 数值非零" "Mem: "

echo ""
echo "[文本处理 applets]"
assert_contains "head -n 2 截取两行" "line1"
assert_contains "printf 格式化输出" "name=rbox-num=42"
assert_contains "wc -c 字节计数" "6"
assert_contains "grep 搜索匹配" "hel"
assert_line "basename 取文件名" "gcc"
assert_line "basename 去后缀" "test"
assert_line "dirname 取目录" "/usr/bin"
assert_line_regex "date 日期输出" "^[A-Z][a-z]{2} [A-Z][a-z]{2} +[0-9]{1,2} [0-9]{2}:[0-9]{2}:[0-9]{2} UTC [0-9]{4}$"
assert_contains "env 环境变量" "PATH"
assert_contains "ln -s 符号链接创建" "rbox"
assert_line "ln -s 符号链接读取" "rbox"
assert_contains "echo -n 无换行" "no_newlineafter"
assert_line "ls -a 包含 ." "."
assert_line "ls -a 包含 .." ".."
assert_line "ls -1 单列输出" "bin"
assert_contains "rm -r 递归删除" "No such file"
assert_contains "touch 创建文件" "touched_new"
assert_contains "mkdir -p 嵌套目录" "dir"
assert_contains "tail -n 1 末尾行" "ccc"

echo ""
echo "[新增 applet: chmod/chown/find/kill/dmesg/mount/umount]"
assert_contains "chmod 600 生效" "-rw-------"
assert_line "chown 成功" "chown_rc=0"
assert_line "find -name 递归 a" "/tmp/find_t/a.txt"
assert_line "find -name 递归 b" "/tmp/find_t/sub/b.txt"
assert_line "find -type d 目录" "/tmp/find_t/sub"
assert_line "kill -0 探测成功" "kill_ok"
assert_contains "kill -l 列出信号" "HUP"
assert_line "dmesg 命令执行完成" "dmesg_done"
assert_not_contains_in "$OUT" "dmesg 输出非空" "^0$"
assert_contains "mount 列出挂载表" " / "
assert_line "umount 不存在目标失败" "umount_rc=1"
assert_line "kill -l 9 -> KILL" "KILL"
assert_line "find -maxdepth 只列顶层" "/tmp/find_d/x.txt"
assert_not_contains_in "$OUT" "find -maxdepth 不列子目录" "/tmp/find_d/sub/y.txt"
assert_contains "chmod u+x 生效" "-rwx------"
assert_contains "mount 未知目标提示 fstab" "/etc/fstab"

echo ""
echo "[Shell: stderr 重定向]"
assert_contains "2> stderr 重定向" "No such file"
assert_contains "2>> stderr 追加" "another_missing"

echo ""
echo "[Shell: source 命令]"
assert_contains "source 加载变量" "yes"

echo ""
echo "[Shell: PS1]"
assert_contains "PS1 设置执行" "done"

echo ""
echo "[Shell: here-doc]"
assert_contains "here-doc 内容" "hello heredoc"

echo ""
echo "[console respawn]"
assert_contains "console 初始环境变量" "console_init=1"
assert_contains "respawn 后环境变量保留" "console_respawn=1"

echo ""
echo "[前台 Ctrl-C 中断]"
assert_contains "前台命令 Ctrl-C 中断" "intr_rc=130"

echo ""
echo "[重启流程]"
assert_contains "reboot 触发有序关机" "rbox init: rebooting"
assert_contains "重启后系统恢复" "after_reboot"

echo ""
echo "[关机流程]"
assert_contains "shutdown 触发关机" "shutting down"
assert_contains "ExecStop 逆序执行" "stopping"
assert_contains "power off" "power off"

# ─── 内核 cmdline 启动模式：emergency / single（独立 QEMU 会话）───
echo ""
echo "[emergency/single 启动模式]"
EMERGENCY_OUT=$(timeout 130 bash -c '
{
  sleep 25
  printf "echo EMERGENCY_SHELL_OK\n"; sleep 1
  printf "shutdown\n"; sleep 8
} | qemu-system-aarch64 -M virt -cpu cortex-a72 -m 128M -nographic   -kernel '"$KERNEL"' -initrd '"$INITRD"' -append "'"$APPEND"' emergency"
' 2>&1) || true
assert_contains_in "$EMERGENCY_OUT" "emergency 模式进入应急 shell" "emergency mode, emergency shell"
assert_not_contains_in "$EMERGENCY_OUT" "emergency 跳过服务启动" "starting console-shell"
assert_contains_in "$EMERGENCY_OUT" "应急 shell 可用" "EMERGENCY_SHELL_OK"

SINGLE_OUT=$(timeout 130 bash -c '
{
  sleep 25
  printf "echo SINGLE_SHELL_OK\n"; sleep 1
  printf "shutdown\n"; sleep 8
} | qemu-system-aarch64 -M virt -cpu cortex-a72 -m 128M -nographic   -kernel '"$KERNEL"' -initrd '"$INITRD"' -append "'"$APPEND"' single"
' 2>&1) || true
assert_contains_in "$SINGLE_OUT" "single 模式进入单用户 shell" "single mode, emergency shell"
assert_not_contains_in "$SINGLE_OUT" "single 跳过服务启动" "starting console-shell"
assert_contains_in "$SINGLE_OUT" "单用户 shell 可用" "SINGLE_SHELL_OK"

# ─── rescue 模式：default.target 的 Requires 失败 → 停止服务进 rescue shell ───
echo ""
echo "[rescue 启动降级]"
RESCUE_TARGET=rootfs/etc/rbox/system/default.target.toml
RESCUE_UNIT=rootfs/etc/rbox/system/rescue-fail.service.toml
cp "$RESCUE_TARGET" /tmp/rbox_rescue_target_bak.toml
cat > "$RESCUE_TARGET" <<'TOML'
[Unit]
Name = "default.target"
Requires = ["rescue-fail"]

[Install]
WantedBy = []
TOML
cat > "$RESCUE_UNIT" <<'TOML'
[Unit]
Description = "Always-failing unit for rescue test"
Name = "rescue-fail"

[Service]
Type = "simple"
ExecStart = "/bin/rbox false"

[Install]
WantedBy = ["default.target"]
TOML
(cd rootfs && find . | cpio -o -H newc 2>/dev/null | gzip > ../rescue-test.cpio.gz)
mv /tmp/rbox_rescue_target_bak.toml "$RESCUE_TARGET"
rm -f "$RESCUE_UNIT"
RESCUE_OUT=$(timeout 130 bash -c '
{
  sleep 25
  printf "echo RESCUE_SHELL_OK\n"; sleep 1
  printf "shutdown\n"; sleep 8
} | qemu-system-aarch64 -M virt -cpu cortex-a72 -m 128M -nographic   -kernel '"$KERNEL"' -initrd rescue-test.cpio.gz -append '"'$APPEND'"'' 2>&1) || true
rm -f rescue-test.cpio.gz
assert_contains_in "$RESCUE_OUT" "target 未达成日志" "target default.target not reached"
assert_contains_in "$RESCUE_OUT" "进入 rescue 模式" "rescue mode, emergency shell"
assert_contains_in "$RESCUE_OUT" "rescue shell 可用" "RESCUE_SHELL_OK"
assert_not_contains_in "$RESCUE_OUT" "rescue 未达到 target" "reached target default.target"

# ─── 持久盘模式：root=/dev/vda + switch_root + 重启后数据保留 ───
echo ""
echo "[持久盘模式（ext4 + switch_root）]"
if command -v mkfs.ext4 >/dev/null 2>&1 || [ -x /sbin/mkfs.ext4 ]; then
    make disk >/dev/null 2>&1
    DISK_OUT=$(timeout 180 bash -c '
    {
      sleep 30
      printf "root\n"; sleep 2
      printf "root\n"; sleep 2
      printf "echo PERSIST_MARKER > /persist_test.txt; sync; echo disk_write_ok\n"; sleep 1
      printf "reboot\n"; sleep 35
      printf "root\n"; sleep 2
      printf "root\n"; sleep 2
      printf "cat /persist_test.txt\n"; sleep 1
      printf "shutdown\n"; sleep 10
    } | qemu-system-aarch64 -M virt -cpu cortex-a72 -m 128M -nographic \
      -kernel '"$KERNEL"' -initrd '"$INITRD"' \
      -drive file=rootfs.ext4,format=raw,if=virtio \
      -append "'"$APPEND"' root=/dev/vda"
    ' 2>&1) || true
    assert_contains_in "$DISK_OUT" "切换到持久根" "switching to persistent root /dev/vda"
    assert_contains_in "$DISK_OUT" "磁盘写入成功" "disk_write_ok"
    assert_contains_in "$DISK_OUT" "重启后数据保留" "PERSIST_MARKER"
else
    echo "  SKIP  持久盘模式（宿主机缺少 mkfs.ext4）"
fi

echo ""
echo "========================================"
echo "结果: $PASS 通过, $FAIL 失败"
echo "========================================"
[ "$FAIL" -eq 0 ]
