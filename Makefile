# Ramag — 常用任务封装
# 默认 target 是 help，避免误触发耗时构建。

.PHONY: help \
        develop release \
        check fmt fmt-check clippy test \
        dmg dmg-x86 dmg-arm64 dmg-universal \
        win-debug \
        clean clean-all \
        deps-update lock-refresh

.DEFAULT_GOAL := help

help:
	@printf "\033[1mRamag — 常用命令\033[0m\n\n"
	@printf "  \033[36m开发\033[0m\n"
	@printf "    make develop        cargo run -p ramag-bin（debug，编译快）\n"
	@printf "    make release        cargo run --release -p ramag-bin（首次 ~2-3 分钟）\n"
	@printf "\n  \033[36m检查\033[0m\n"
	@printf "    make check          cargo check --all-targets\n"
	@printf "    make fmt            cargo fmt --all\n"
	@printf "    make fmt-check      cargo fmt --all -- --check（CI 用）\n"
	@printf "    make clippy         cargo clippy --all-targets -- -D warnings\n"
	@printf "    make test           cargo test --all\n"
	@printf "\n  \033[36m打包（macOS）\033[0m\n"
	@printf "    make dmg            当前架构：svg → icns → cargo build → Ramag.app → Ramag.dmg\n"
	@printf "    make dmg-x86        交叉编译 Intel mac\n"
	@printf "    make dmg-arm64      交叉编译 Apple Silicon\n"
	@printf "    make dmg-universal  Intel + Apple Silicon 通用二进制（约 2 倍编译时间）\n"
	@printf "\n  \033[36mWindows x64\033[0m\n"
	@printf "    make win-debug      macOS 交叉构建 debug（用于编译验证）\n"
	@printf "    build-windows.ps1   Windows 原生构建 debug / release（-Release）\n"
	@printf "    （当前输出便携 exe；安装包与签名属于发布流程）\n"
	@printf "\n  \033[36m清理\033[0m\n"
	@printf "    make clean          cargo clean\n"
	@printf "    make clean-all      clean + 删 Ramag.app / dmg / icns\n"
	@printf "\n  \033[36m依赖\033[0m\n"
	@printf "    make deps-update    cargo update\n"
	@printf "    make lock-refresh   删除 Cargo.lock 重新解析（git 依赖会拉最新 master）\n"

# === 开发 ============================================================
develop:
	cargo run -p ramag-bin

release:
	cargo run --release -p ramag-bin

# === 检查 ============================================================
check:
	cargo check --all-targets

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

clippy:
	cargo clippy --all-targets -- -D warnings

test:
	cargo test --all

# === 打包（macOS）====================================================
# build-dmg.sh 内部：svg→icns、cargo build、组装 .app、打 dmg 全流程。
# 交叉编译目标若未安装 rustup target，脚本会自动 rustup target add。
dmg:
	./scripts/build-dmg.sh

dmg-x86:
	./scripts/build-dmg.sh --target=x86_64

dmg-arm64:
	./scripts/build-dmg.sh --target=arm64

dmg-universal:
	./scripts/build-dmg.sh --target=universal

# === 跨编（macOS → Windows）==========================================
# 在 macOS 上直接编出 debug ramag.exe（x64），无需 Windows 机器。脚本内含前置依赖检查
# （cargo-xwin / brew llvm / rust target）与 GPUI 清单资源、lld-link 的修复。
# 只出 x64：Windows on ARM 靠内置 x64 模拟即可运行，一个包覆盖几乎所有用户。
win-debug:
	./scripts/build-windows-local.sh

# === 清理 ============================================================
clean:
	cargo clean

clean-all: clean
	rm -rf target/Ramag*.app
	rm -rf target/dmg-staging*
	rm -f target/Ramag*.dmg
	rm -f scripts/icons/ramag.icns

# === 依赖 ============================================================
deps-update:
	cargo update

lock-refresh:
	rm -f Cargo.lock
	cargo check
