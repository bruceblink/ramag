#!/usr/bin/env bash
# 在 Linux x86_64 上构建、校验并生成 deb 与 AppImage。

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
LINUX_DIR="$SCRIPT_DIR/linux"
BUILD_DIR="$REPO_DIR/target/linux-package"
DIST_DIR="$REPO_DIR/target/linux-dist"
TOOLS_DIR="$REPO_DIR/target/linux-tools"
APP_ID="com.ramag.Ramag"
LINUXDEPLOY_URL="https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage"
LINUXDEPLOY_SHA256="c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d"
APPIMAGE_RUNTIME_URL="https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-x86_64"
APPIMAGE_RUNTIME_SHA256="1cc49bcf1e2ccd593c379adb17c9f85a36d619088296504de95b1d06215aebbf"

# shellcheck source=scripts/linux/release-lib.sh
source "$LINUX_DIR/release-lib.sh"

fail() {
    echo "$1" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "Required command is missing: $1"
}

download_verified_tool() {
    local url="$1" expected_sha="$2" destination="$3"
    if [[ -f "$destination" ]] && printf '%s  %s\n' "$expected_sha" "$destination" | sha256sum --check --status; then
        chmod 0755 "$destination"
        return
    fi
    local partial="${destination}.part"
    rm -f "$partial"
    curl --fail --location --retry 3 --output "$partial" "$url"
    printf '%s  %s\n' "$expected_sha" "$partial" | sha256sum --check --status || {
        rm -f "$partial"
        fail "Downloaded tool checksum mismatch: $url"
    }
    mv "$partial" "$destination"
    chmod 0755 "$destination"
}

[[ "$(uname -s)" == "Linux" ]] || fail "Linux packages must be built on Linux."
[[ "$(uname -m)" == "x86_64" ]] || fail "Only Linux x86_64 packaging is supported."
for command in cargo curl dpkg-deb file install jq mksquashfs readelf sed sha256sum xz; do
    require_command "$command"
done

version="$(linux_get_app_version "$REPO_DIR")"
linux_assert_tag_matches_version "$version"
deb_name="$(linux_get_deb_asset_name "$version")"
appimage_name="$(linux_get_appimage_asset_name "$version")"

cd "$REPO_DIR"
cargo build --locked --release -p ramag-bin
binary="$REPO_DIR/target/release/ramag"
[[ -x "$binary" ]] || fail "Release binary was not created."
readelf -h "$binary" | grep -Fq 'Advanced Micro Devices X86-64' || \
    fail "Release binary is not Linux x86_64."

rm -rf "$BUILD_DIR" "$DIST_DIR"
mkdir -p "$BUILD_DIR" "$DIST_DIR" "$TOOLS_DIR"

deb_root="$BUILD_DIR/deb-root"
install -Dm755 "$binary" "$deb_root/usr/bin/ramag"
install -Dm644 "$LINUX_DIR/$APP_ID.desktop" \
    "$deb_root/usr/share/applications/$APP_ID.desktop"
install -Dm644 "$SCRIPT_DIR/icons/ramag.svg" \
    "$deb_root/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg"
install -Dm644 "$REPO_DIR/LICENSE" "$deb_root/usr/share/doc/ramag/LICENSE"
installed_size="$(du -sk "$deb_root/usr" | awk '{print $1}')"
mkdir -p "$deb_root/DEBIAN"
sed -e "s/@VERSION@/$version/g" -e "s/@INSTALLED_SIZE@/$installed_size/g" \
    "$LINUX_DIR/debian-control.in" >"$deb_root/DEBIAN/control"
dpkg-deb --root-owner-group -Zxz -z9 --build "$deb_root" "$DIST_DIR/$deb_name"
dpkg-deb --info "$DIST_DIR/$deb_name" >/dev/null
dpkg-deb --contents "$DIST_DIR/$deb_name" >"$BUILD_DIR/deb-contents.txt"
grep -Fq './usr/bin/ramag' "$BUILD_DIR/deb-contents.txt"

linuxdeploy="${LINUXDEPLOY_PATH:-$TOOLS_DIR/linuxdeploy-x86_64.AppImage}"
appimage_runtime="$TOOLS_DIR/runtime-x86_64"
if [[ -z "${LINUXDEPLOY_PATH:-}" ]]; then
    download_verified_tool "$LINUXDEPLOY_URL" "$LINUXDEPLOY_SHA256" "$linuxdeploy"
fi
download_verified_tool "$APPIMAGE_RUNTIME_URL" "$APPIMAGE_RUNTIME_SHA256" "$appimage_runtime"
[[ -x "$linuxdeploy" ]] || fail "linuxdeploy is not executable: $linuxdeploy"

app_dir="$BUILD_DIR/Ramag.AppDir"
appimage_icon="$BUILD_DIR/$APP_ID.svg"
install -Dm644 "$SCRIPT_DIR/icons/ramag.svg" "$appimage_icon"
APPIMAGE_EXTRACT_AND_RUN=1 "$linuxdeploy" \
    --appdir "$app_dir" \
    --executable "$binary" \
    --desktop-file "$LINUX_DIR/$APP_ID.desktop" \
    --icon-file "$appimage_icon"
install -Dm644 "$REPO_DIR/LICENSE" "$app_dir/usr/share/doc/ramag/LICENSE"
appimage_squashfs="$BUILD_DIR/Ramag.squashfs"
mksquashfs "$app_dir" "$appimage_squashfs" \
    -noappend -all-root -comp zstd -b 131072 >/dev/null
cp "$appimage_runtime" "$DIST_DIR/$appimage_name"
cat "$appimage_squashfs" >>"$DIST_DIR/$appimage_name"
chmod 0755 "$DIST_DIR/$appimage_name"
file "$DIST_DIR/$appimage_name" | grep -Fq 'ELF 64-bit'
appimage_extract_dir="$BUILD_DIR/appimage-extract"
mkdir -p "$appimage_extract_dir"
(
    cd "$appimage_extract_dir"
    "$DIST_DIR/$appimage_name" --appimage-extract >/dev/null
    [[ -x squashfs-root/AppRun ]]
    [[ -x squashfs-root/usr/bin/ramag ]]
    [[ -f "squashfs-root/$APP_ID.desktop" ]]
    [[ -f "squashfs-root/$APP_ID.svg" ]]
)

(
    cd "$DIST_DIR"
    sha256sum "$deb_name" "$appimage_name" >SHA256SUMS.txt
    sha256sum --check SHA256SUMS.txt
)

echo "Linux packages created in $DIST_DIR"
