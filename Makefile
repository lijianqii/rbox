# rbox - 构建与运行 Makefile
#
# 目标：
#   make build    - 交叉编译 rbox (aarch64-unknown-linux-gnu, release)
#   make rootfs   - 拷贝 rbox + glibc 运行时到 rootfs/
#   make initramfs - 打包 rootfs/ 为 initramfs.cpio.gz
#   make run      - QEMU 全系统模拟启动
#   make test     - 集成测试（自动化 QEMU 验证）
#   make kernel   - 编译 ARM64 内核 (defconfig + Image)
#   make clean    - 清理构建产物
#   make all      - build + rootfs + initramfs

TARGET   := aarch64-unknown-linux-gnu
PROFILE  := release
ROOTFS   := rootfs
KERNEL   := kernel
INITRD   := initramfs.cpio.gz
QEMU     := qemu-system-aarch64
QEMU_OPTS := -M virt -cpu cortex-a72 -m 512M -nographic
QEMU_KCMD := console=ttyAMA0 rdinit=/init

# glibc 运行时（从交叉编译器的 multiarch 库目录拷贝）
# gcc -print-file-name 能解析出实际的 libc 路径，比 -print-sysroot 更可靠
GLIBC_DIR := $(shell dirname $(shell aarch64-linux-gnu-gcc -print-file-name=libc.so.6 2>/dev/null))

# applet 列表（与 src/applet.rs APPLETS 一致）
APPLETS := true false echo cat pwd uname init shell ls cp mv rm mkdir touch shutdown reboot \
          head tail wc grep ln date sleep env printf basename dirname status

.PHONY: all build rootfs initramfs run test unittest kernel clean help

all: build rootfs initramfs

# ─── 交叉编译 ────────────────────────────────────
build:
	cargo build --target $(TARGET) --$(PROFILE)

# ─── 构建 rootfs ─────────────────────────────────
rootfs: build
	mkdir -p $(ROOTFS)/bin
	# 拷贝 rbox 二进制
	cp target/$(TARGET)/$(PROFILE)/rbox $(ROOTFS)/bin/rbox
	# 创建 applet 符号链接（bin/<applet> -> rbox）
	@for app in $(APPLETS); do \
		ln -sf rbox $(ROOTFS)/bin/$$app; \
	done
	# init 符号链接
	ln -sf bin/rbox $(ROOTFS)/init
	# 拷贝 glibc 运行时（缺失时报错，避免打包出不可启动的 initramfs）
	@if [ -z "$(GLIBC_DIR)" ] || [ ! -f "$(GLIBC_DIR)/libc.so.6" ]; then \
		echo "错误: 找不到 glibc 运行时 ($(GLIBC_DIR))，请确认已安装 gcc-aarch64-linux-gnu" >&2; exit 1; \
	fi
	mkdir -p $(ROOTFS)/lib/aarch64-linux-gnu
	cp -L $(GLIBC_DIR)/ld-linux-aarch64.so.1 $(ROOTFS)/lib/
	cp -L $(GLIBC_DIR)/libc.so.6 $(ROOTFS)/lib/aarch64-linux-gnu/
	cp -L $(GLIBC_DIR)/libgcc_s.so.1 $(ROOTFS)/lib/aarch64-linux-gnu/
	@echo "rootfs 构建完成"

# ─── 打包 initramfs ──────────────────────────────
initramfs: rootfs
	cd $(ROOTFS) && find . | cpio -o -H newc 2>/dev/null | gzip > ../$(INITRD)
	@echo "initramfs 打包完成: $(INITRD) ($$(du -h $(INITRD) | cut -f1))"

# ─── QEMU 运行 ───────────────────────────────────
run: initramfs
	@if [ ! -f $(KERNEL)/arch/arm64/boot/Image ]; then \
		echo "错误: 内核镜像不存在，请先 make kernel" >&2; exit 1; \
	fi
	$(QEMU) $(QEMU_OPTS) \
		-kernel $(KERNEL)/arch/arm64/boot/Image \
		-initrd $(INITRD) \
		-append "$(QEMU_KCMD)"

# ─── 单元测试（宿主机）──────────────────────────
unittest:
	cargo test --target x86_64-unknown-linux-gnu

# ─── 集成测试 ────────────────────────────────────
test: initramfs
	@echo "运行集成测试..."
	@bash tests/run_tests.sh

# ─── 内核编译 ────────────────────────────────────
kernel:
	cd $(KERNEL) && make ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu- defconfig
	cd $(KERNEL) && make ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu- -j$$(nproc) Image

# ─── 清理 ────────────────────────────────────────
clean:
	cargo clean
	rm -f $(INITRD)

help:
	@echo "rbox 构建系统"
	@echo ""
	@echo "目标:"
	@echo "  make all       - 编译 + rootfs + initramfs"
	@echo "  make build     - 交叉编译 rbox"
	@echo "  make rootfs    - 构建 rootfs（含符号链接 + glibc）"
	@echo "  make initramfs - 打包 initramfs.cpio.gz"
	@echo "  make run       - QEMU 启动"
	@echo "  make test      - 集成测试"
	@echo "  make unittest  - 宿主机单元测试 (x86_64)"
	@echo "  make kernel    - 编译 ARM64 内核"
	@echo "  make clean     - 清理产物"
