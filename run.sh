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

exec qemu-system-aarch64 \
    -M virt \
    -cpu cortex-a72 \
    -m 512M \
    -nographic \
    -kernel "$KERNEL" \
    -initrd "$INITRD" \
    -append "console=ttyAMA0 rdinit=/init"
