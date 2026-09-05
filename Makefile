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
# 内核版本与源码下载源（本地无 tarball 时从清华开源镜像站自动下载）
KERNEL_VERSION := 6.12.36
KERNEL_TARBALL := linux-$(KERNEL_VERSION).tar.xz
KERNEL_URL := https://mirrors.tuna.tsinghua.edu.cn/kernel/v6.x/$(KERNEL_TARBALL)
# 可选：设置后下载/复用 tarball 前做 sha256 校验（供应链加固；留空则跳过）
KERNEL_SHA256 ?=
QEMU     := qemu-system-aarch64
QEMU_OPTS := -M virt -cpu cortex-a72 -m 128M -nographic
QEMU_KCMD := console=ttyAMA0 rdinit=/init

# 持久化 rootfs（ext4 磁盘镜像，make disk 生成）
DISK      := rootfs.ext4
DISK_SIZE ?= 64M
MKFS      := $(shell command -v mkfs.ext4 2>/dev/null || echo /sbin/mkfs.ext4)

TEST_INITRD := initramfs.test.cpio.gz
TEST_UNITS := tests/units/*.toml

# glibc 运行时（从交叉编译器的 multiarch 库目录拷贝）
# gcc -print-file-name 能解析出实际的 libc 路径，比 -print-sysroot 更可靠
GLIBC_DIR := $(shell dirname $(shell aarch64-linux-gnu-gcc -print-file-name=libc.so.6 2>/dev/null))

# applet 列表：从 rbox --list 自动提取，避免与 src/applet.rs 手动同步
APPLETS := $(shell cargo run --target x86_64-unknown-linux-gnu --quiet -- --list 2>/dev/null)

.PHONY: all build strip rootfs initramfs rootfs-test run run-disk disk test unittest verify fmt kernel clean help

all: build rootfs initramfs

# ─── 交叉编译 ────────────────────────────────────
build:
	cargo build --target $(TARGET) --$(PROFILE)

# ─── strip 符号表（减小体积）─────────────────────
strip: build
	aarch64-linux-gnu-strip --strip-all target/$(TARGET)/$(PROFILE)/rbox
	@echo "strip 完成: $$(du -h target/$(TARGET)/$(PROFILE)/rbox | cut -f1)"

# ─── 构建 rootfs ─────────────────────────────────
rootfs: strip
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
	# libcrypt（rlogin 的 crypt 密码校验依赖），用 -print-file-name 单独解析
	CRYPT_LIB=$$(aarch64-linux-gnu-gcc -print-file-name=libcrypt.so.1 2>/dev/null); \
	if [ -z "$$CRYPT_LIB" ] || [ ! -f "$$CRYPT_LIB" ]; then \
		echo "错误: 找不到 libcrypt.so.1（aarch64-linux-gnu-gcc -print-file-name），rlogin 的 crypt 校验将无法工作" >&2; exit 1; \
	fi; \
	cp -L $$CRYPT_LIB $(ROOTFS)/lib/aarch64-linux-gnu/
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
	@if [ -t 0 ]; then saved_tty=$$(stty -g 2>/dev/null); \
		stty -echo -icanon min 1 time 0 2>/dev/null; \
		trap 'stty $$saved_tty 2>/dev/null' EXIT INT; \
	fi
	$(QEMU) $(QEMU_OPTS) \
		-kernel $(KERNEL)/arch/arm64/boot/Image \
		-initrd $(INITRD) \
		-append "$(QEMU_KCMD)"
	@if [ -t 0 ]; then stty sane 2>/dev/null; fi

# ─── 构建测试用 initramfs（含测试服务单元）─────────
# 注入 tests/units/*.toml 到 rootfs，打包独立的测试 initramfs，
# 然后清理注入的文件，生产 rootfs 保持干净。
rootfs-test: rootfs
	@echo "注入测试服务单元..."
	# 备份会被测试单元覆盖的生产单元（如 console-shell），打包后恢复
	@for f in $(notdir $(wildcard $(TEST_UNITS))); do \
		if [ -f $(ROOTFS)/etc/rbox/system/$$f ]; then \
			cp $(ROOTFS)/etc/rbox/system/$$f /tmp/rbox_unit_bak_$$f; \
		fi; \
	done
	cp $(TEST_UNITS) $(ROOTFS)/etc/rbox/system/
	cd $(ROOTFS) && find . | cpio -o -H newc 2>/dev/null | gzip > ../$(TEST_INITRD)
	@echo "清理注入的测试服务..."
	@for f in $(notdir $(wildcard $(TEST_UNITS))); do \
		rm -f $(ROOTFS)/etc/rbox/system/$$f; \
		if [ -f /tmp/rbox_unit_bak_$$f ]; then \
			mv /tmp/rbox_unit_bak_$$f $(ROOTFS)/etc/rbox/system/$$f; \
		fi; \
	done
	@echo "测试 initramfs 构建完成: $(TEST_INITRD) ($$(du -h $(TEST_INITRD) | cut -f1))"

# ─── 持久化 rootfs（ext4 磁盘镜像）────────────────
# 制作 rootfs.ext4：initramfs 的 init 检测到 root=/dev/vda 内核参数后，
# 会挂载该设备并 switch_root 到持久 rootfs（见 src/applets/core/init/mod.rs）。
disk: rootfs
	@if [ ! -x "$(MKFS)" ]; then echo "错误: 缺少 mkfs.ext4（e2fsprogs）" >&2; exit 1; fi
	dd if=/dev/zero of=$(DISK) bs=1M count=$(shell echo $(DISK_SIZE) | tr -d M) 2>/dev/null
	$(MKFS) -q -d $(ROOTFS) $(DISK)
	@echo "ext4 磁盘镜像构建完成: $(DISK) ($$(du -h $(DISK) | cut -f1))"

# 从 ext4 磁盘镜像启动（root= 触发 init 的 switch_root）
run-disk: disk
	@if [ ! -f $(KERNEL)/arch/arm64/boot/Image ]; then \
		echo "错误: 内核镜像不存在，请先 make kernel" >&2; exit 1; \
	fi
	$(QEMU) $(QEMU_OPTS) \
		-kernel $(KERNEL)/arch/arm64/boot/Image \
		-initrd $(INITRD) \
		-drive file=$(DISK),format=raw,if=virtio \
		-append "$(QEMU_KCMD) root=/dev/vda"

# ─── 单元测试（宿主机）──────────────────────────
unittest:
	cargo test --target x86_64-unknown-linux-gnu

# ─── 集成测试 ────────────────────────────────────
test: initramfs
	@echo "运行集成测试..."
	@bash tests/run_tests.sh

# ─── 内核编译 ────────────────────────────────────
# 幂等：
#   1) 若 kernel/ 源码不存在（无 Makefile），先用 xz -t 校验根目录已有的 tarball，
#      校验通过则复用，损坏/缺失则自动从清华开源镜像站下载并解压到 kernel/
#      （--strip-components=1 去掉顶层目录）；若设置 KERNEL_SHA256 则额外做 sha256 校验
#   2) 若 .config 不存在才生成 defconfig（避免覆盖已有配置）
#   3) 若 Image 不存在才编译（已存在则跳过）
kernel:
	@if [ ! -f $(KERNEL)/Makefile ]; then \
		if [ -f $(KERNEL_TARBALL) ] && xz -t $(KERNEL_TARBALL) >/dev/null 2>&1; then \
			echo "使用本地内核源码包: $(KERNEL_TARBALL)"; \
		else \
			[ -f $(KERNEL_TARBALL) ] && echo "本地内核包损坏/缺失，重新下载" || echo "内核源码缺失，从清华镜像站下载: $(KERNEL_URL)"; \
			rm -f $(KERNEL_TARBALL); \
			(wget -q -O $(KERNEL_TARBALL) $(KERNEL_URL) || curl -fsSL -o $(KERNEL_TARBALL) $(KERNEL_URL)) || { echo "下载失败" >&2; exit 1; }; \
		fi; \
		if [ -n "$(KERNEL_SHA256)" ]; then \
			echo "校验内核源码包 (sha256) ..."; \
			echo "$(KERNEL_SHA256)  $(KERNEL_TARBALL)" | sha256sum -c - >/dev/null 2>&1 || { echo "校验和不符，已删除，请重新运行 make kernel" >&2; rm -f $(KERNEL_TARBALL); exit 1; }; \
		fi; \
		echo "解压内核源码到 $(KERNEL)/ ..."; \
		tar -xf $(KERNEL_TARBALL) -C $(KERNEL) --strip-components=1 || { echo "解压失败" >&2; rm -f $(KERNEL_TARBALL); exit 1; }; \
	fi
	@if [ ! -f $(KERNEL)/.config ]; then \
		echo "生成内核默认配置 (defconfig) ..."; \
		cd $(KERNEL) && $(MAKE) ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu- defconfig; \
	else \
		echo "内核配置已存在，跳过 defconfig"; \
	fi
	@if [ ! -f $(KERNEL)/arch/arm64/boot/Image ]; then \
		echo "编译内核镜像 (Image) ..."; \
		cd $(KERNEL) && $(MAKE) ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu- -j$$(nproc) Image; \
	else \
		echo "内核镜像已存在: $(KERNEL)/arch/arm64/boot/Image"; \
	fi

# ─── 格式检查 ──────────────────────────────────────
fmt:
	cargo fmt --check

# ─── 验证（CI 用）────────────────────────────────
verify: check clippy fmt unittest

check:
	cargo check --target x86_64-unknown-linux-gnu

clippy:
	cargo clippy --target x86_64-unknown-linux-gnu -- -D warnings

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
	@echo "  make strip     - strip 符号表减小体积"
	@echo "  make rootfs    - 构建 rootfs（含符号链接 + glibc）"
	@echo "  make initramfs - 打包 initramfs.cpio.gz"
	@echo "  make rootfs-test - 构建含测试单元的 initramfs（不污染生产 rootfs）"
	@echo "  make disk       - 制作 ext4 磁盘镜像 (rootfs.ext4)"
	@echo "  make run-disk   - 从 ext4 磁盘镜像启动（持久 rootfs）"
	@echo "  make run        - QEMU 启动（initramfs）"
	@echo "  make test      - 集成测试"
	@echo "  make unittest  - 宿主机单元测试 (x86_64)"
	@echo "  make fmt       - cargo fmt --check 格式检查"
	@echo "  make verify    - check + clippy + fmt + unittest（CI 用）"
	@echo "  make kernel    - 编译 ARM64 内核（源码缺失时自动从清华镜像下载）"
	@echo "  make clean     - 清理产物"
