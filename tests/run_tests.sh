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
OUT=$(timeout 120 bash -c '
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
  # Shell 增强：变量、控制操作符
  printf "export FOO=bar\n"; sleep 0.5
  printf "echo \$FOO\n"; sleep 0.5
  printf "echo a; echo b\n"; sleep 0.5
  printf "true && echo yes\n"; sleep 0.5
  printf "false || echo fallback\n"; sleep 0.5
  printf "echo rc=\$?\n"; sleep 0.5
  # Tab 补全
  printf "ec\thello\n"; sleep 0.5
  printf "cat /etc/host\t\n"; sleep 0.5
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
assert_contains "kmsg 日志写入" "\\] rbox: rbox init: mounting devpts"


echo ""
echo "[Shell 增强]"
assert_contains "export + 变量展开" "bar"
assert_contains "命令分隔 ;" "^a"
assert_contains "条件执行 &&" "yes"
assert_contains "条件执行 ||" "fallback"


echo ""
echo "[Tab 补全]"
assert_contains "命令补全 ec->echo" "echo hello"
assert_contains "文件补全 /etc/host->hostname" "rbox"


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
