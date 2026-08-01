#!/bin/bash
# rbox 集成测试脚本
# 在 QEMU 全系统模拟中运行预设命令并验证输出
set -e

cd "$(dirname "$0")/.."

KERNEL=kernel/arch/arm64/boot/Image
INITRD=initramfs.cpio.gz
QEMU="qemu-system-aarch64 -M virt -cpu cortex-a72 -m 512M -nographic"
APPEND="console=ttyAMA0 rdinit=/init"

PASS=0
FAIL=0

# 单次 QEMU 运行所有测试命令
# 命令序列本身约 25 秒，超时需留足内核启动余量（负载高时启动会变慢）
OUT=$(timeout 90 bash -c '
{
  sleep 8
  # 基本 applet
  printf "uname -m\n"; sleep 0.5
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
  # 关机
  printf "shutdown\n"; sleep 10
} | qemu-system-aarch64 -M virt -cpu cortex-a72 -m 512M -nographic \
  -kernel '"$KERNEL"' -initrd '"$INITRD"' -append "'"$APPEND"'"
' 2>&1) || true

# 断言输出包含某字符串
assert_contains() {
    local desc="$1"
    local pattern="$2"
    if echo "$OUT" | grep -q "$pattern"; then
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
assert_contains "pwd -> /" "^/"
assert_contains "echo hello -> hello" "hello"
assert_contains "cat /etc/hostname" "hello from"

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
echo "[关机流程]"
assert_contains "shutdown 触发关机" "shutting down"
assert_contains "ExecStop 逆序执行" "stopping"
assert_contains "power off" "power off"

echo ""
echo "========================================"
echo "结果: $PASS 通过, $FAIL 失败"
echo "========================================"
[ "$FAIL" -eq 0 ]
