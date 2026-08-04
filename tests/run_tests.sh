#!/bin/bash
# rbox 集成测试脚本
# 在 QEMU 全系统模拟中运行预设命令并验证输出
#
# 测试专用服务（tests/units/*.toml）在运行时注入 rootfs 并打包独立的
# 测试 initramfs，测试结束后自动清理，生产 rootfs 保持干净。
set -e

cd "$(dirname "$0")/.."

KERNEL=kernel/arch/arm64/boot/Image
INITRD=initramfs.cpio.gz
TEST_INITRD=initramfs.test.cpio.gz
UNITS_DIR=rootfs/etc/rbox/system
QEMU="qemu-system-aarch64 -M virt -cpu cortex-a72 -m 512M -nographic"
APPEND="console=ttyAMA0 rdinit=/init"

PASS=0
FAIL=0

# ─── 注入测试服务 + 打包测试 initramfs ───────────────
cp tests/units/*.toml "$UNITS_DIR"/
(cd rootfs && find . | cpio -o -H newc 2>/dev/null | gzip > ../$TEST_INITRD)

cleanup() {
    rm -f "$TEST_INITRD"
    # 仅删除注入的测试服务（不触碰生产配置）
    rm -f "$UNITS_DIR/hello.service.toml" "$UNITS_DIR/restart-test.service.toml" \
        "$UNITS_DIR/longrun.service.toml" "$UNITS_DIR/forktest.service.toml" \
        "$UNITS_DIR/forktimeout.service.toml" "$UNITS_DIR/usertest.service.toml"
}
trap cleanup EXIT

# 单次 QEMU 运行所有测试命令
# 命令序列本身约 30 秒，超时需留足内核启动余量（负载高时启动会变慢）
# 注意：整个命令块在外层 bash -c '...' 单引号中，内部 printf 必须用双引号，
#       $ 需转义为 \$，单引号用 \x27 代替，避免破坏外层引号。
OUT=$(timeout 180 bash -c '
{
  sleep 12
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
  # 11. 文本处理 applets
  printf "echo -e 'line1\\nline2\\nline3' | head -n 2\n"; sleep 0.5
  printf "printf name=%%s-num=%%d rbox 42\n"; sleep 0.5
  printf "echo hello | wc -c\n"; sleep 0.5
  printf "echo hello | grep -o hel\n"; sleep 0.5
  printf "basename /usr/bin/gcc\n"; sleep 0.5
  printf "basename /tmp/test.txt .txt\n"; sleep 0.5
  printf "dirname /usr/bin/gcc\n"; sleep 0.5
  printf "date | head -c 3\n"; sleep 0.5
  # 12. env / ln
  printf "env | head -n 1\n"; sleep 0.5
  printf "ln -s /etc/hostname /tmp/linktest\n"; sleep 0.5
  printf "cat /tmp/linktest\n"; sleep 0.5
  # 13. echo -n
  printf "echo -n no_newline; echo after\n"; sleep 0.5
  # 14. ls -a / ls -1
  printf "ls -a / | head -n 3\n"; sleep 0.5
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
  printf "echo -e 'aaa\\nbbb\\nccc' | tail -n 1\n"; sleep 0.5
  # 关机
  printf "shutdown\n"; sleep 10
} | qemu-system-aarch64 -M virt -cpu cortex-a72 -m 512M -nographic \
  -kernel '"$KERNEL"' -initrd '"$TEST_INITRD"' -append "'"$APPEND"'"
' 2>&1) || true

# 断言输出包含某字符串（QEMU 串口输出为 CRLF，先去除 \r 再匹配）
assert_contains() {
    local desc="$1"
    local pattern="$2"
    if echo "$OUT" | tr -d '\r' | grep -q "$pattern"; then
        echo "  PASS  $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL  $desc (期望包含: '$pattern')"
        FAIL=$((FAIL + 1))
    fi
}

echo "========================================"
echo "rbox 集成测试"
echo "========================================"
echo ""

echo "[基本 applet]"
assert_contains "uname -m -> aarch64" "aarch64"
assert_contains "uname -n -> 主机名" "^rbox$"
assert_contains "pwd -> /" "^/"
assert_contains "echo hello -> hello" "hello"
assert_contains "cat /etc/hostname" "^rbox$"

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
assert_contains "status 列出 console" "console-shell"
assert_contains "status 列出重启服务" "restart-test"
assert_contains "status 单服务查询" "^hello "
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
assert_contains "status 单查 console" "^console-shell running"
assert_contains "sysctl kernel.panic" "^10$"
assert_contains "User= 降权 nobody" "65534"
assert_contains "Type=forking 等待父进程" "started forktest"
assert_contains "Type=forking 超时终止" "did not daemonize within 2s"
assert_contains "kmsg 日志写入" "\] rbox: rbox init: mounting devpts"


echo ""
echo "[Shell: 引号与转义]"
assert_contains "双引号保留空格" "hello world"
assert_contains "单引号原样保留" "single quoted"
assert_contains "反斜杠转义" "hello world"
assert_contains "续行拼接" "line1 more"
assert_contains "注释不执行" "visible"

echo ""
echo "[Shell: 变量展开]"
assert_contains "export + \$VAR" "^bar$"
assert_contains "\${VAR}_x 展开" "bar_x"
assert_contains "\$? 退出码" "rc=1"
assert_contains "\$\$ PID 展开" "pid="
assert_contains "unset 后为空" "^\[\]$"

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
echo "[Shell: 历史扩展]"
assert_contains "!! 重复上一条" "hist_two"
assert_contains "!n 第n条命令" "hist_one"
assert_contains "!$ 最后参数" "copy:three"
assert_contains "history 内置命令" "hist_one"

echo ""
echo "[Shell: ~ 展开]"
assert_contains "echo ~ 输出 HOME" "^/$"
assert_contains "cd ~ 后 pwd" "^/$"

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
echo "[文本处理 applets]"
assert_contains "head -n 2 截取两行" "line1"
assert_contains "printf 格式化输出" "name=rbox-num=42"
assert_contains "wc -c 字节计数" "6"
assert_contains "grep 搜索匹配" "hel"
assert_contains "basename 取文件名" "gcc"
assert_contains "basename 去后缀" "test"
assert_contains "dirname 取目录" "/usr/bin"
assert_contains "date 日期输出" "20"
assert_contains "env 环境变量" "PATH"
assert_contains "ln -s 符号链接创建" "rbox"
assert_contains "ln -s 符号链接读取" "rbox"
assert_contains "echo -n 无换行" "no_newlineafter"
assert_contains "ls -a 包含 ." "\."
assert_contains "ls -1 单列输出" "bin"
assert_contains "rm -r 递归删除" "No such file"
assert_contains "touch 创建文件" "touched_new"
assert_contains "mkdir -p 嵌套目录" "dir"
assert_contains "tail -n 1 末尾行" "ccc"

echo ""
echo "[关机流程]"
assert_contains "shutdown 触发关机" "shutting down"
assert_contains "ExecStop 逆序执行" "stopping"
assert_contains "power off" "power off"

echo ""
echo "========================================"
echo "结果: $PASS 通过, $FAIL 失败"
echo "========================================"
[ "$FAIL" -eq 0 ]
