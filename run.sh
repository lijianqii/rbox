#!/bin/bash
# rbox - QEMU 全系统模拟启动脚本
set -e

cd "$(dirname "$0")"

KERNEL=kernel/arch/arm64/boot/Image
INITRD=initramfs.cpio.gz

if [ ! -f "$KERNEL" ]; then
    echo "错误: 内核镜像 $KERNEL 不存在，请先编译内核" >&2
    exit 1
fi
if [ ! -f "$INITRD" ]; then
    echo "错误: initramfs $INITRD 不存在，请先构建 rootfs" >&2
    exit 1
fi

echo "启动 QEMU (ARM64)..."
echo "  内核:   $KERNEL"
echo "  initrd: $INITRD"
echo "  退出: Ctrl-A X"
echo "----------------------------------------"

# 设置终端为 raw 模式，使 Tab 补全等按键能立即传递给 QEMU
if [ -t 0 ]; then
    stty -echo -icanon min 1 time 0 2>/dev/null
    trap 'stty sane 2>/dev/null' EXIT INT
fi

qemu-system-aarch64 \
    -M virt \
    -cpu cortex-a72 \
    -m 128M \
    -nographic \
    -kernel "$KERNEL" \
    -initrd "$INITRD" \
    -append "console=ttyAMA0 rdinit=/init"

# QEMU 退出后 trap 会自动恢复终端
