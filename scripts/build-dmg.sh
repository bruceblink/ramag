#!/usr/bin/env bash
# 一条龙：svg → icns → cargo build → Ramag.app → Ramag.dmg
#
# 依赖（macOS 自带）：
#   - sips、iconutil（Xcode CLT 提供 iconutil；svg 转 png 用 sips）
#   - hdiutil（DMG 打包）
#   - cargo（项目自带 rust-toolchain.toml）
#
# 用法：
#   ./scripts/build-dmg.sh                     # release，当前架构（native）
#   ./scripts/build-dmg.sh --debug             # debug 二进制（更快编译，dmg 体积大）
#   ./scripts/build-dmg.sh --target=x86_64     # 交叉编译到 Intel mac
#   ./scripts/build-dmg.sh --target=arm64      # 交叉编译到 Apple Silicon
#
# 产物（带架构后缀，避免互相覆盖）：
#   - native：    target/Ramag.app    / target/Ramag.dmg
#   - x86_64：    target/Ramag-x86_64.app    / target/Ramag-x86_64.dmg
#   - arm64：     target/Ramag-arm64.app     / target/Ramag-arm64.dmg

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RELEASE_LIB="$SCRIPT_DIR/macos/release-lib.sh"

# shellcheck source=macos/release-lib.sh
source "$RELEASE_LIB"

# === 参数解析 ========================================================
PROFILE="release"
TARGET="native"
for arg in "$@"; do
    case "$arg" in
        --debug)    PROFILE="debug" ;;
        --target=*) TARGET="${arg#--target=}" ;;
        -h|--help)
            sed -n '1,25p' "$0"
            exit 0
            ;;
        *)
            echo "Unknown argument: $arg" >&2
            echo "Run $0 --help for usage." >&2
            exit 1
            ;;
    esac
done

# 把 --target 标准化成 cargo target triple（"" 代表 native）
case "$TARGET" in
    native)
        TARGET_TRIPLE=""
        SUFFIX=""
        ;;
    x86_64|intel|x86_64-apple-darwin)
        TARGET_TRIPLE="x86_64-apple-darwin"
        SUFFIX="-x86_64"
        ;;
    arm64|aarch64|aarch64-apple-darwin)
        TARGET_TRIPLE="aarch64-apple-darwin"
        SUFFIX="-arm64"
        ;;
    *)
        echo "Unsupported target '$TARGET'. Use native, x86_64, or arm64." >&2
        exit 1
        ;;
esac

if [[ "$PROFILE" == "release" ]]; then
    CARGO_FLAGS=(--release)
    PROFILE_DIR="release"
else
    # Bash 3.2 + nounset 不能安全展开空数组，显式指定 dev profile。
    CARGO_FLAGS=(--profile dev)
    PROFILE_DIR="debug"
fi

# === 路径 ============================================================
ICON_SOURCE_DIR="$SCRIPT_DIR/icons"
SVG="$ICON_SOURCE_DIR/ramag.svg"
ICON_WORK_DIR="$REPO_DIR/target/macos-icon${SUFFIX}"
ICONSET="$ICON_WORK_DIR/ramag.iconset"
ICNS="$ICON_WORK_DIR/ramag.icns"

APP="$REPO_DIR/target/Ramag${SUFFIX}.app"
DMG="$REPO_DIR/target/Ramag${SUFFIX}.dmg"
STAGING="$REPO_DIR/target/dmg-staging${SUFFIX}"

cleanup_build_temp() {
    rm -rf "$STAGING" "$ICON_WORK_DIR"
}
trap cleanup_build_temp EXIT

# === 依赖检查 ========================================================
NEED_CMDS=(sips iconutil hdiutil codesign cargo jq rustup)
for cmd in "${NEED_CMDS[@]}"; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        echo "Required command not found: $cmd" >&2
        case "$cmd" in
            iconutil|lipo) echo "Install Xcode Command Line Tools with xcode-select --install." >&2 ;;
            cargo|rustup)  echo "Install Rust with rustup: https://rustup.rs" >&2 ;;
            jq)            echo "Install jq with Homebrew: brew install jq" >&2 ;;
        esac
        exit 1
    fi
done

if [[ ! -f "$SVG" ]]; then
    echo "Application SVG icon is missing: $SVG" >&2
    exit 1
fi
if [[ ! -f "$REPO_DIR/LICENSE" ]]; then
    echo "Project license is missing: $REPO_DIR/LICENSE" >&2
    exit 1
fi

APP_VERSION="$(macos_get_app_version "$REPO_DIR")"
BUNDLE_VERSION="$(macos_get_bundle_version "$APP_VERSION")"
export MACOSX_DEPLOYMENT_TARGET="12.0"

# 确认 rustup target 已安装；缺失则 rustup target add（按 rust-toolchain.toml 当前 toolchain）
ensure_target_installed() {
    local triple="$1"
    if ! rustup target list --installed 2>/dev/null | grep -qx "$triple"; then
        echo "Installing Rust target: $triple"
        rustup target add "$triple"
    fi
}

if [[ -n "$TARGET_TRIPLE" ]]; then
    ensure_target_installed "$TARGET_TRIPLE"
fi

# === 1) svg → icns ===================================================
# 中间图标只写入 target，避免在源码目录留下构建缓存。
echo "Step 1/4: generating ICNS icon"
rm -rf "$ICON_WORK_DIR"
mkdir -p "$ICONSET"

# Apple iconset 标准尺寸：16/32/64/128/256/512/1024，含 @2x
sips -s format png -Z 16   "$SVG" --out "$ICONSET/icon_16x16.png"      >/dev/null
sips -s format png -Z 32   "$SVG" --out "$ICONSET/icon_16x16@2x.png"   >/dev/null
sips -s format png -Z 32   "$SVG" --out "$ICONSET/icon_32x32.png"      >/dev/null
sips -s format png -Z 64   "$SVG" --out "$ICONSET/icon_32x32@2x.png"   >/dev/null
sips -s format png -Z 128  "$SVG" --out "$ICONSET/icon_128x128.png"    >/dev/null
sips -s format png -Z 256  "$SVG" --out "$ICONSET/icon_128x128@2x.png" >/dev/null
sips -s format png -Z 256  "$SVG" --out "$ICONSET/icon_256x256.png"    >/dev/null
sips -s format png -Z 512  "$SVG" --out "$ICONSET/icon_256x256@2x.png" >/dev/null
sips -s format png -Z 512  "$SVG" --out "$ICONSET/icon_512x512.png"    >/dev/null
sips -s format png -Z 1024 "$SVG" --out "$ICONSET/icon_512x512@2x.png" >/dev/null

iconutil -c icns "$ICONSET" -o "$ICNS"
rm -rf "$ICONSET"

# === 2) cargo build ==================================================
cd "$REPO_DIR"

if [[ -z "$TARGET_TRIPLE" ]]; then
    # native：不带 --target，产物在 target/$PROFILE_DIR/
    echo "Step 2/4: building ramag-bin for the native architecture ($PROFILE)"
    cargo build --locked "${CARGO_FLAGS[@]}" -p ramag-bin
    BIN_PATH="$REPO_DIR/target/$PROFILE_DIR/ramag"
else
    echo "Step 2/4: building ramag-bin for $TARGET_TRIPLE ($PROFILE)"
    cargo build --locked "${CARGO_FLAGS[@]}" --target="$TARGET_TRIPLE" -p ramag-bin
    BIN_PATH="$REPO_DIR/target/$TARGET_TRIPLE/$PROFILE_DIR/ramag"
fi

# === 3) 组装 Ramag.app ===============================================
echo "Step 3/4: assembling $(basename "$APP")"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
mkdir -p "$APP/Contents/Resources"

cp "$BIN_PATH" "$APP/Contents/MacOS/Ramag"
cp "$ICNS" "$APP/Contents/Resources/ramag.icns"
cp "$REPO_DIR/LICENSE" "$APP/Contents/Resources/LICENSE"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>Ramag</string>
    <key>CFBundleDisplayName</key>
    <string>Ramag</string>
    <key>CFBundleIdentifier</key>
    <string>com.axemc.ramag</string>
    <key>CFBundleVersion</key>
    <string>${BUNDLE_VERSION}</string>
    <key>CFBundleShortVersionString</key>
    <string>${BUNDLE_VERSION}</string>
    <key>RamagCargoVersion</key>
    <string>${APP_VERSION}</string>
    <key>CFBundleExecutable</key>
    <string>Ramag</string>
    <key>CFBundleIconFile</key>
    <string>ramag</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>LSMinimumSystemVersion</key>
    <string>12.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.developer-tools</string>
</dict>
</plist>
EOF

# adhoc 签名整个 bundle（必须在 bundle 内容全部就位后、打包前做）：
# 1) 消除「linker 只签了 Mach-O、bundle 没有 _CodeSignature」的不一致（codesign --verify 告警）
# 2) 满足 Apple Silicon 对可执行文件必须签名的要求
# 注意：adhoc 签名无法通过 Gatekeeper 公证校验，传输后仍会被打隔离标记，需 xattr 解除（见文末）
echo "Applying ad hoc code signature..."
codesign --force --sign - "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"

# === 4) 打包成 DMG ===================================================
echo "Step 4/4: creating $(basename "$DMG")"
rm -rf "$STAGING"
mkdir -p "$STAGING"
cp -R "$APP" "$STAGING/Ramag.app"
ln -s /Applications "$STAGING/Applications"
rm -f "$DMG"

hdiutil create \
    -volname "Ramag" \
    -srcfolder "$STAGING" \
    -fs HFS+ \
    -format UDZO \
    -imagekey zlib-level=9 \
    "$DMG" >/dev/null

rm -rf "$STAGING"
hdiutil verify "$DMG" >/dev/null

echo ""
echo "DMG created successfully:"
ls -lh "$DMG"
if [[ -n "$TARGET_TRIPLE" ]]; then
    echo "Architecture: ${TARGET_TRIPLE}"
fi
echo "Version: $APP_VERSION"
echo "Signature: ad hoc (not notarized)"
echo ""
echo "Test with: open $DMG"
echo "Drag Ramag.app to Applications after mounting the DMG."
echo ""
echo "This build is not notarized and may be blocked by Gatekeeper after download."
echo "For local testing only, remove quarantine with:"
echo "  xattr -dr com.apple.quarantine /Applications/Ramag.app"
