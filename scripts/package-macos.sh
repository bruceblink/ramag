#!/usr/bin/env bash
# macOS 双架构 Release 打包入口；分别生成 ARM64、Intel DMG 与 SHA-256。

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BUILD_SCRIPT="$SCRIPT_DIR/build-dmg.sh"
RELEASE_LIB="$SCRIPT_DIR/macos/release-lib.sh"
DIST_DIR="$REPO_DIR/target/macos-dist"
WORK_DIR="$REPO_DIR/target/macos-package"
CURRENT_MOUNT_DIR=""
CURRENT_MOUNTED=false

# shellcheck source=macos/release-lib.sh
source "$RELEASE_LIB"

fail() {
    echo "$1" >&2
    exit 1
}

assert_command() {
    local command_name="$1"
    command -v "$command_name" >/dev/null 2>&1 ||
        fail "Required command not found: $command_name"
}

assert_file() {
    local path="$1"
    local description="$2"
    [[ -f "$path" ]] || fail "$description is missing: $path"
    [[ -s "$path" ]] || fail "$description is empty: $path"
}

read_plist_value() {
    local plist="$1"
    local key="$2"
    /usr/libexec/PlistBuddy -c "Print :$key" "$plist"
}

cleanup_current_mount() {
    if [[ "$CURRENT_MOUNTED" == true && -n "$CURRENT_MOUNT_DIR" ]]; then
        hdiutil detach "$CURRENT_MOUNT_DIR" -quiet ||
            echo "Failed to detach temporary DMG mount: $CURRENT_MOUNT_DIR" >&2
    fi
}

verify_app_bundle() {
    local app="$1"
    local version="$2"
    local bundle_version="$3"
    local expected_architecture="$4"
    local plist="$app/Contents/Info.plist"
    local executable="$app/Contents/MacOS/Ramag"
    local actual_architectures
    local minimum_versions

    assert_file "$plist" "Application Info.plist"
    assert_file "$executable" "Application executable"
    assert_file "$app/Contents/Resources/ramag.icns" "Application icon"
    assert_file "$app/Contents/Resources/LICENSE" "Application license"
    plutil -lint "$plist" >/dev/null

    [[ "$(read_plist_value "$plist" "CFBundleIdentifier")" == "com.axemc.ramag" ]] ||
        fail "Unexpected macOS bundle identifier."
    [[ "$(read_plist_value "$plist" "CFBundleShortVersionString")" == "$bundle_version" ]] ||
        fail "macOS short version does not match Cargo version $version."
    [[ "$(read_plist_value "$plist" "CFBundleVersion")" == "$bundle_version" ]] ||
        fail "macOS bundle version does not match Cargo version $version."
    [[ "$(read_plist_value "$plist" "RamagCargoVersion")" == "$version" ]] ||
        fail "macOS Cargo version metadata does not match $version."

    actual_architectures="$(lipo -archs "$executable")"
    [[ "$actual_architectures" == "$expected_architecture" ]] ||
        fail "Unexpected macOS architecture: expected $expected_architecture, got $actual_architectures."
    minimum_versions="$(
        vtool -show-build "$executable" |
            awk '$1 == "minos" { print $2 }' |
            sort -u
    )"
    [[ "$minimum_versions" == "12.0" ]] ||
        fail "Unexpected macOS deployment target: ${minimum_versions:-missing}"
    codesign --verify --deep --strict --verbose=2 "$app"
}

package_architecture() {
    local architecture="$1"
    local version="$2"
    local bundle_version="$3"
    local source_app="$REPO_DIR/target/Ramag-$architecture.app"
    local source_dmg="$REPO_DIR/target/Ramag-$architecture.dmg"
    local output_name
    local output_dmg

    bash "$BUILD_SCRIPT" "--target=$architecture"
    assert_file "$source_dmg" "$architecture macOS DMG"
    verify_app_bundle "$source_app" "$version" "$bundle_version" "$architecture"

    output_name="$(macos_get_release_asset_name "$version" "$architecture")"
    output_dmg="$DIST_DIR/$output_name"
    cp "$source_dmg" "$output_dmg"
    assert_file "$output_dmg" "$architecture macOS release DMG"
    hdiutil verify "$output_dmg" >/dev/null

    CURRENT_MOUNT_DIR="$WORK_DIR/mount-$architecture"
    mkdir -p "$CURRENT_MOUNT_DIR"
    hdiutil attach \
        "$output_dmg" \
        -readonly \
        -nobrowse \
        -mountpoint "$CURRENT_MOUNT_DIR" \
        >/dev/null
    CURRENT_MOUNTED=true
    verify_app_bundle \
        "$CURRENT_MOUNT_DIR/Ramag.app" \
        "$version" \
        "$bundle_version" \
        "$architecture"
    [[ -L "$CURRENT_MOUNT_DIR/Applications" ]] ||
        fail "DMG Applications shortcut is missing."
    [[ "$(readlink "$CURRENT_MOUNT_DIR/Applications")" == "/Applications" ]] ||
        fail "DMG Applications shortcut has an unexpected target."
    hdiutil detach "$CURRENT_MOUNT_DIR" -quiet
    CURRENT_MOUNTED=false
    rmdir "$CURRENT_MOUNT_DIR"
    CURRENT_MOUNT_DIR=""
}

main() {
    local version
    local bundle_version
    local arm_asset
    local intel_asset
    local checksum_path

    [[ "$(uname -s)" == "Darwin" ]] ||
        fail "This script must run on macOS. macOS release packages are built by GitHub Actions."
    for command_name in cargo jq hdiutil lipo plutil vtool codesign shasum; do
        assert_command "$command_name"
    done
    assert_file "$BUILD_SCRIPT" "macOS build script"

    version="$(macos_get_app_version "$REPO_DIR")"
    bundle_version="$(macos_get_bundle_version "$version")"
    macos_assert_tag_matches_version "$version"

    rm -rf "$DIST_DIR" "$WORK_DIR"
    mkdir -p "$DIST_DIR" "$WORK_DIR"
    trap cleanup_current_mount EXIT

    package_architecture "arm64" "$version" "$bundle_version"
    package_architecture "x86_64" "$version" "$bundle_version"

    arm_asset="$(macos_get_release_asset_name "$version" "arm64")"
    intel_asset="$(macos_get_release_asset_name "$version" "x86_64")"
    checksum_path="$DIST_DIR/SHA256SUMS.txt"
    (
        cd "$DIST_DIR"
        shasum -a 256 "$arm_asset" "$intel_asset" >SHA256SUMS.txt
        shasum -a 256 -c SHA256SUMS.txt
    )
    assert_file "$checksum_path" "SHA-256 checksum file"

    rmdir "$WORK_DIR"
    trap - EXIT

    echo "macOS package completed: $DIST_DIR"
    find "$DIST_DIR" -maxdepth 1 -type f -print | sort
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main "$@"
fi
