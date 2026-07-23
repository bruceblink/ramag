#!/usr/bin/env bash
# 在 macOS 上交叉编译 Windows x64 可执行文件（ramag.exe），无需 Windows 机器。
#
# 用法：
#   ./scripts/build-windows-local.sh            # Release：构建期预编译着色器
#   ./scripts/build-windows-local.sh --debug    # Debug：从 exe 内嵌源码运行时编译着色器
#   ./scripts/build-windows-local.sh --release  # 显式构建 Release
#
# 通用依赖（缺失时脚本会提示如何安装）：
#   - cargo-xwin：cargo install cargo-xwin --locked
#   - Homebrew llvm：brew install llvm
#   - rustup target：x86_64-pc-windows-msvc（脚本自动安装）
# Release 额外需要以下二选一：
#   - 设置 GPUI_FXC_PATH，指向能接收 POSIX 路径的 macOS FXC 包装器；或
#   - 安装 Wine，并设置 RAMAG_FXC_EXE 指向 Windows SDK 的 x64/fxc.exe。
#
# 本项目只发布 x64 包；Windows on ARM 可通过系统自带的 x64 模拟运行。
set -euo pipefail

TARGET="x86_64-pc-windows-msvc"
EXPECTED_GPUI_SOURCE="git+https://github.com/zed-industries/zed#cb9dc8daf17bd5b053beabfb02fe0a0b2e3bd9f4"
EXPECTED_GPUI_BUILD_RS_SHA256="3192734d0448f3ec44525e3b55c025d30d9926edce6b24e5575ac27a1ab3ad06"
EXPECTED_GPUI_RENDERER_SHA256="03cb1e9fb0bd18ff2f0e7445275e26f4376239d08cfea67509382cda00fa0f21"

fail() {
    echo "$1" >&2
    exit 1
}

# Cargo 的 rustc 包装模式：只替换编译输入，不改写 gpui_windows 源码。
run_rustc_wrapper() {
    [ "$#" -ge 1 ] || fail "rustc wrapper did not receive the rustc path"
    local rustc="$1"
    shift
    local arg source_hash
    local -a rustc_args=()

    for arg in "$@"; do
        case "$arg" in
            */crates/gpui_windows/build.rs)
                if [ -n "${RAMAG_GPUI_BUILD_RS:-}" ]; then
                    : "${RAMAG_GPUI_EXPECTED_BUILD_RS_SHA256:?RAMAG_GPUI_EXPECTED_BUILD_RS_SHA256 is required}"
                    [ -f "$arg" ] || fail "gpui_windows build script not found: $arg"
                    source_hash="$(shasum -a 256 "$arg" | awk '{print $1}')"
                    [ "$source_hash" = "$RAMAG_GPUI_EXPECTED_BUILD_RS_SHA256" ] || {
                        fail "gpui_windows build.rs changed; update the macOS Release bridge before building"
                    }
                    rustc_args+=("$RAMAG_GPUI_BUILD_RS")
                else
                    rustc_args+=("$arg")
                fi
                ;;
            */crates/gpui_windows/src/gpui_windows.rs)
                if [ -n "${RAMAG_GPUI_DEBUG_CRATE_ROOT:-}" ]; then
                    [ -f "$RAMAG_GPUI_DEBUG_CRATE_ROOT" ] || {
                        fail "patched gpui_windows crate root not found: $RAMAG_GPUI_DEBUG_CRATE_ROOT"
                    }
                    rustc_args+=("$RAMAG_GPUI_DEBUG_CRATE_ROOT")
                else
                    rustc_args+=("$arg")
                fi
                ;;
            *)
                rustc_args+=("$arg")
                ;;
        esac
    done

    if [ -n "${RAMAG_INNER_RUSTC_WRAPPER:-}" ]; then
        exec "$RAMAG_INNER_RUSTC_WRAPPER" "$rustc" "${rustc_args[@]}"
    fi
    exec "$rustc" "${rustc_args[@]}"
}

# Debug 版需要运行时编译着色器；将源码嵌入临时副本，避免携带构建机绝对路径。
configure_debug_bridge() {
    local gpui_source revision checkout_revision
    local source_dir renderer patch_file patch_hash patched_dir
    local shader
    local -a source_dirs=()

    gpui_source="$(locked_package_source gpui_windows)"
    [ "$gpui_source" = "$EXPECTED_GPUI_SOURCE" ] || {
        fail "gpui_windows 已升级（当前：${gpui_source:-未知}），请同步更新 macOS Debug 构建桥接"
    }

    revision="${EXPECTED_GPUI_SOURCE##*#}"
    checkout_revision="${revision:0:7}"
    source_dirs=(
        "$HOME"/.cargo/git/checkouts/zed-*/"$checkout_revision"/crates/gpui_windows
    )
    [ "${#source_dirs[@]}" -eq 1 ] && [ -d "${source_dirs[0]}" ] || {
        fail "找不到锁定的 gpui_windows 源码目录（revision=$checkout_revision）"
    }
    source_dir="${source_dirs[0]}"
    renderer="$source_dir/src/directx_renderer.rs"
    patch_file="$REPO_DIR/scripts/windows/gpui-windows-debug.patch"

    [ -f "$patch_file" ] || fail "缺少 GPUI Debug 构建补丁：$patch_file"
    [ -f "$renderer" ] || fail "找不到 GPUI DirectX 渲染器源码：$renderer"
    [ "$(shasum -a 256 "$renderer" | awk '{print $1}')" = "$EXPECTED_GPUI_RENDERER_SHA256" ] || {
        fail "gpui_windows DirectX 渲染器已变化，请同步更新 macOS Debug 构建桥接"
    }

    patch_hash="$(shasum -a 256 "$patch_file" "$REPO_DIR/scripts/build-windows-local.sh" \
        | shasum -a 256 | awk '{print substr($1, 1, 16)}')"
    patched_dir="$REPO_DIR/target/xwin-gpui-debug/$patch_hash"
    if [ ! -f "$patched_dir/.ready" ]; then
        mkdir -p "$patched_dir"
        cp -R "$source_dir/src" "$patched_dir/"
        patch --batch -s -d "$patched_dir" -p1 <"$patch_file" || {
            fail "应用 GPUI Debug 构建补丁失败"
        }

        for shader in shaders color_text_raster; do
            [ "$(sed -n '1p' "$source_dir/src/$shader.hlsl")" = '#include "alpha_correction.hlsl"' ] || {
                fail "GPUI $shader.hlsl 的 include 结构已变化，请更新 Debug 构建桥接"
            }
            awk '
                NR == FNR { print; next }
                FNR > 1 { print }
            ' \
                "$source_dir/src/alpha_correction.hlsl" \
                "$source_dir/src/$shader.hlsl" \
                >"$patched_dir/src/${shader}_embedded.hlsl"
        done
        touch "$patched_dir/.ready"
    fi

    export RAMAG_GPUI_DEBUG_CRATE_ROOT="$patched_dir/src/gpui_windows.rs"
    export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }--cfg=ramag_gpui_debug_bridge_$patch_hash"
    if [ -n "${RUSTC_WRAPPER:-}" ]; then
        export RAMAG_INNER_RUSTC_WRAPPER="$RUSTC_WRAPPER"
    fi
    export RUSTC_WRAPPER="$XBIN/ramag-rustc-wrapper-$patch_hash"
    ln -sf "$REPO_DIR/scripts/build-windows-local.sh" "$RUSTC_WRAPPER"
}

# GPUI 的 FXC 包装模式：把 macOS 路径转换成 Wine 能访问的 Windows 路径。
run_fxc_wrapper() {
    : "${RAMAG_WINE:?RAMAG_WINE is required}"
    : "${RAMAG_WINEPATH:?RAMAG_WINEPATH is required}"
    : "${RAMAG_FXC_EXE:?RAMAG_FXC_EXE is required}"

    local arg converted
    local -a fxc_args=()
    while [ "$#" -gt 0 ]; do
        arg="$1"
        shift
        if [ "$arg" = "/Fh" ]; then
            [ "$#" -gt 0 ] || fail "fxc /Fh is missing its output path"
            converted="$("$RAMAG_WINEPATH" -w "$1")" || fail "winepath failed for output: $1"
            converted="${converted//$'\r'/}"
            fxc_args+=("/Fh" "$converted")
            shift
        elif [ -f "$arg" ]; then
            converted="$("$RAMAG_WINEPATH" -w "$arg")" || fail "winepath failed for input: $arg"
            converted="${converted//$'\r'/}"
            fxc_args+=("$converted")
        else
            fxc_args+=("$arg")
        fi
    done

    exec "$RAMAG_WINE" "$RAMAG_FXC_EXE" "${fxc_args[@]}"
}

case "$(basename "$0")" in
    ramag-rustc-wrapper-*) run_rustc_wrapper "$@" ;;
    ramag-fxc-wrapper-*) run_fxc_wrapper "$@" ;;
esac

PROFILE="release"
RELEASE=true
case "$#" in
    0) ;;
    1)
        case "$1" in
            --release) ;;
            --debug)
                PROFILE="debug"
                RELEASE=false
                ;;
            *)
                echo "用法：./scripts/build-windows-local.sh [--release|--debug]" >&2
                exit 2
                ;;
        esac
        ;;
    *)
        echo "用法：./scripts/build-windows-local.sh [--release|--debug]" >&2
        exit 2
        ;;
esac

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_DIR"

# cargo install 默认把子命令放在 Cargo home；Homebrew rustup 不一定将它加入 PATH。
CARGO_INSTALL_BIN="${CARGO_HOME:-$HOME/.cargo}/bin"
if [ -d "$CARGO_INSTALL_BIN" ]; then
    export PATH="$CARGO_INSTALL_BIN:$PATH"
fi

resolve_executable() {
    local candidate="$1"
    if [[ "$candidate" == */* ]]; then
        if [ -x "$candidate" ]; then
            printf '%s/%s\n' "$(cd "$(dirname "$candidate")" && pwd)" "$(basename "$candidate")"
        fi
    else
        command -v "$candidate" 2>/dev/null || true
    fi
    return 0
}

find_wine_fxc() {
    local wine_prefix="${WINEPREFIX:-$HOME/.wine}"
    local candidate latest=""
    for candidate in \
        "$wine_prefix/drive_c/Program Files (x86)/Windows Kits/10/bin/"*/x64/fxc.exe \
        "$wine_prefix/drive_c/Program Files/Windows Kits/10/bin/"*/x64/fxc.exe; do
        [ -f "$candidate" ] || continue
        if [ -z "$latest" ] || [[ "$candidate" > "$latest" ]]; then
            latest="$candidate"
        fi
    done
    [ -n "$latest" ] && printf '%s\n' "$latest"
}

locked_package_source() {
    awk -v package="$1" '
        $0 == "[[package]]" { matched = 0 }
        $0 == "name = \"" package "\"" { matched = 1; next }
        matched && /^source = / {
            sub(/^source = \"/, "")
            sub(/\"$/, "")
            print
            exit
        }
    ' Cargo.lock
}

configure_release_bridge() {
    local gpui_source fxc_exe wine winepath patch_hash
    local patched_build_rs="$REPO_DIR/scripts/windows/gpui-windows-build.rs"

    gpui_source="$(locked_package_source gpui_windows)"
    [ "$gpui_source" = "$EXPECTED_GPUI_SOURCE" ] || {
        fail "gpui_windows 已升级（当前：${gpui_source:-未知}），请同步更新 macOS Release 构建桥接"
    }
    [ -f "$patched_build_rs" ] || fail "缺少 GPUI Release 构建桥接：$patched_build_rs"

    patch_hash="$(shasum -a 256 "$patched_build_rs" "$REPO_DIR/scripts/build-windows-local.sh" \
        | shasum -a 256 | awk '{print substr($1, 1, 16)}')"
    export RAMAG_GPUI_BUILD_RS="$patched_build_rs"
    export RAMAG_GPUI_EXPECTED_BUILD_RS_SHA256="$EXPECTED_GPUI_BUILD_RS_SHA256"

    if [ -n "${RUSTC_WRAPPER:-}" ]; then
        export RAMAG_INNER_RUSTC_WRAPPER="$RUSTC_WRAPPER"
    fi
    export RUSTC_WRAPPER="$XBIN/ramag-rustc-wrapper-$patch_hash"
    ln -sf "$REPO_DIR/scripts/build-windows-local.sh" "$RUSTC_WRAPPER"

    if [ -n "${GPUI_FXC_PATH:-}" ]; then
        GPUI_FXC_PATH="$(resolve_executable "$GPUI_FXC_PATH")"
        [ -n "$GPUI_FXC_PATH" ] || fail "GPUI_FXC_PATH 不存在或不可执行"
        export GPUI_FXC_PATH
        echo "使用自定义 FXC 包装器：$GPUI_FXC_PATH"
        return
    fi

    fxc_exe="${RAMAG_FXC_EXE:-}"
    [ -n "$fxc_exe" ] || fxc_exe="$(find_wine_fxc || true)"
    [ -f "$fxc_exe" ] || {
        fail "找不到 fxc.exe。请通过 Wine 安装 Windows SDK，并设置 RAMAG_FXC_EXE=/path/to/x64/fxc.exe"
    }
    fxc_exe="$(cd "$(dirname "$fxc_exe")" && pwd)/$(basename "$fxc_exe")"

    wine="$(resolve_executable "${RAMAG_WINE:-wine64}")"
    [ -n "$wine" ] || wine="$(resolve_executable wine)"
    [ -n "$wine" ] || fail "找不到 Wine。请安装 Wine/CrossOver，或设置 RAMAG_WINE"

    winepath="$(resolve_executable "${RAMAG_WINEPATH:-winepath}")"
    if [ -z "$winepath" ] && [ -x "$(dirname "$wine")/winepath" ]; then
        winepath="$(dirname "$wine")/winepath"
    fi
    [ -n "$winepath" ] || fail "找不到 winepath。请安装完整 Wine，或设置 RAMAG_WINEPATH"
    "$wine" --version >/dev/null 2>&1 || fail "Wine 无法运行；Apple Silicon Mac 请确认已安装 Rosetta 2"

    export RAMAG_FXC_EXE="$fxc_exe"
    export RAMAG_WINE="$wine"
    export RAMAG_WINEPATH="$winepath"
    export GPUI_FXC_PATH="$XBIN/ramag-fxc-wrapper-$patch_hash"
    ln -sf "$REPO_DIR/scripts/build-windows-local.sh" "$GPUI_FXC_PATH"
    echo "使用 Wine FXC：$RAMAG_FXC_EXE"
}

# 1) 前置检查
command -v cargo >/dev/null 2>&1 || fail "缺 cargo，请先通过 rustup 安装 Rust"
command -v rustup >/dev/null 2>&1 || fail "缺 rustup，请先安装：https://rustup.rs"
command -v cargo-xwin >/dev/null 2>&1 || fail "缺 cargo-xwin，请先：cargo install cargo-xwin --locked"
command -v brew >/dev/null 2>&1 || fail "缺 Homebrew，请先安装后执行：brew install llvm"

LLVM_PREFIX="$(brew --prefix llvm 2>/dev/null || true)"
LLVM_BIN="$LLVM_PREFIX/bin"
LLVM_READY=true
for TOOL in clang-cl llvm-lib llvm-rc llvm-readobj; do
    [ -x "$LLVM_BIN/$TOOL" ] || LLVM_READY=false
done
[ "$LLVM_READY" = true ] || fail "缺 Homebrew llvm（需 clang-cl/llvm-lib/llvm-rc/llvm-readobj），请先：brew install llvm"

rustup target list --installed | grep -q "^$TARGET$" || rustup target add "$TARGET"

# 2) lld-link：用 Rust 自带的 rust-lld 充当 COFF 链接器。
HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
RUSTLLD="$(rustc --print sysroot)/lib/rustlib/$HOST_TRIPLE/bin/rust-lld"
[ -x "$RUSTLLD" ] || fail "找不到 rust-lld：$RUSTLLD"

XBIN="$REPO_DIR/target/xbin"
mkdir -p "$XBIN"
ln -sf "$RUSTLLD" "$XBIN/lld-link"
export PATH="$XBIN:$LLVM_BIN:$PATH"

# 3) 修复 GPUI Windows 清单在 llvm-rc 下的相对路径解析。
shopt -s nullglob
MANIFESTS=("$HOME"/.cargo/git/checkouts/zed-*/*/crates/gpui/resources/windows/gpui.manifest.xml)
if [ "${#MANIFESTS[@]}" -eq 0 ]; then
    echo "首次构建：正在拉取锁定依赖……"
    cargo fetch --locked --target "$TARGET"
    MANIFESTS=("$HOME"/.cargo/git/checkouts/zed-*/*/crates/gpui/resources/windows/gpui.manifest.xml)
fi
[ "${#MANIFESTS[@]}" -gt 0 ] || fail "依赖拉取完成，但仍未找到 GPUI Windows 清单"
for MANIFEST in "${MANIFESTS[@]}"; do
    RES_DIR="$(dirname "$MANIFEST")"
    mkdir -p "$RES_DIR/resources/windows"
    ln -sf ../../gpui.manifest.xml "$RES_DIR/resources/windows/gpui.manifest.xml"
done

if [ "$RELEASE" = true ]; then
    configure_release_bridge
else
    configure_debug_bridge
fi

# 4) 跨平台编译
echo "开始跨编 Windows x64（profile=${PROFILE}）——首次含 GPUI 较慢，之后使用缓存……"
CARGO_ARGS=(build --locked --target "$TARGET" -p ramag-bin)
[ "$RELEASE" = true ] && CARGO_ARGS+=(--release)
cargo xwin "${CARGO_ARGS[@]}"

# 5) 校验产物架构、子系统和 CRT 链接方式。
EXE="target/$TARGET/$PROFILE/ramag.exe"
[ -f "$EXE" ] || fail "构建结束，但未生成预期文件：$EXE"

HEADERS="$("$LLVM_BIN/llvm-readobj" --file-headers "$EXE")"
grep -q "Format: COFF-x86-64" <<<"$HEADERS" || fail "产物不是 Windows x64 PE：$EXE"
if [ "$RELEASE" = true ]; then
    grep -q "Subsystem: IMAGE_SUBSYSTEM_WINDOWS_GUI" <<<"$HEADERS" || fail "Release 产物不是 Windows GUI 子系统：$EXE"
else
    grep -q "Subsystem: IMAGE_SUBSYSTEM_WINDOWS_CUI" <<<"$HEADERS" || fail "Debug 产物不是 Windows 控制台子系统：$EXE"
    LC_ALL=C grep -a -q "float color_brightness" "$EXE" || {
        fail "Debug 产物未嵌入 GPUI 着色器源码：$EXE"
    }
fi

IMPORTS="$("$LLVM_BIN/llvm-readobj" --coff-imports "$EXE")"
if grep -Eiq 'Name: (VCRUNTIME|MSVCP|api-ms-win-crt-)' <<<"$IMPORTS"; then
    fail "Windows 可执行文件仍依赖动态 MSVC/CRT，发布构建校验失败"
fi

echo ""
echo "完成：$REPO_DIR/$EXE"
file "$EXE" 2>/dev/null || true
echo "拷到 Windows x64（Intel/AMD）即可运行。"
