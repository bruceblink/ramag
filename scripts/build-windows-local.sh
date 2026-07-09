#!/usr/bin/env bash
# 在 macOS 上交叉编译出 Windows x64 可执行文件（ramag.exe），无需 Windows 机器。
#
# 用法：
#   ./scripts/build-windows-local.sh            # x64 debug（快，体积大）
#   ./scripts/build-windows-local.sh --release  # x64 release（优化 + 无控制台窗）
#
# 前置依赖（缺失时脚本会提示如何装）：
#   - cargo-xwin：cargo install cargo-xwin（自动下载 Windows SDK/CRT）
#   - Homebrew llvm：brew install llvm（提供 clang-cl / llvm-lib / llvm-rc）
#   - rustup target：x86_64-pc-windows-msvc（脚本自动 add）
# lld-link 由 Rust 自带的 rust-lld 顶替（brew llvm 不含 lld-link）。
#
# 注：本项目只出 x64 包。Windows on ARM 机器靠内置 x64 模拟可直接运行 x64 exe，
# 一个 x64 包即覆盖几乎所有 Windows 用户，故不单独做 arm64 原生构建。
set -euo pipefail

TARGET=x86_64-pc-windows-msvc
PROFILE=debug
BUILD_FLAG=""
if [ "${1:-}" = "--release" ]; then
    PROFILE=release
    BUILD_FLAG="--release"
fi

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_DIR"

# 1) 前置检查
command -v cargo-xwin >/dev/null 2>&1 || {
    echo "缺 cargo-xwin，请先：cargo install cargo-xwin" >&2
    exit 1
}
LLVM_BIN="$(brew --prefix llvm 2>/dev/null)/bin"
[ -x "$LLVM_BIN/clang-cl" ] || {
    echo "缺 Homebrew llvm（需 clang-cl/llvm-lib/llvm-rc），请先：brew install llvm" >&2
    exit 1
}
rustup target list --installed | grep -q "^$TARGET$" || rustup target add "$TARGET"

# 2) lld-link：用当前工具链的 rust-lld 顶替（按调用名分身为 COFF 链接器）
HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
RUSTLLD="$(rustc --print sysroot)/lib/rustlib/$HOST_TRIPLE/bin/rust-lld"
[ -x "$RUSTLLD" ] || {
    echo "找不到 rust-lld：$RUSTLLD" >&2
    exit 1
}
XBIN="$REPO_DIR/target/xbin"
mkdir -p "$XBIN"
ln -sf "$RUSTLLD" "$XBIN/lld-link"
export PATH="$XBIN:$LLVM_BIN:$PATH"

# 3) 修 GPUI 的 windows 清单资源相对路径：embed_resource 跑 llvm-rc 时把 cwd 设为 .rc
#    的父目录（resources/windows/），而 gpui.rc 里写的是相对 crate 根的
#    "resources/windows/gpui.manifest.xml"，双重拼路径找不到。造嵌套软链让它解析。
MANIFEST="$(find "$HOME/.cargo/git/checkouts"/zed-*/*/crates/gpui/resources/windows/gpui.manifest.xml 2>/dev/null | head -1)"
if [ -n "$MANIFEST" ]; then
    RES_DIR="$(dirname "$MANIFEST")"
    mkdir -p "$RES_DIR/resources/windows"
    ln -sf ../../gpui.manifest.xml "$RES_DIR/resources/windows/gpui.manifest.xml"
else
    echo "警告：未找到 gpui 清单文件，若编译在 gpui build.rs 处失败，请检查 gpui 依赖是否已拉取" >&2
fi

# 4) 跨编
echo "开始跨编 x64（profile=${PROFILE}）——首次含 GPUI 较慢，之后走缓存……"
cargo xwin build --target "$TARGET" -p ramag-bin $BUILD_FLAG

EXE="target/$TARGET/$PROFILE/ramag.exe"
echo ""
echo "完成：$REPO_DIR/$EXE"
file "$EXE" 2>/dev/null || true
echo "拷到 Windows（x64 / Intel / AMD）即可运行。"
