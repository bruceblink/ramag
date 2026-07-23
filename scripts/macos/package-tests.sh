#!/usr/bin/env bash
# macOS 打包纯逻辑回归测试，不构建应用或挂载 DMG。

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=release-lib.sh
source "$SCRIPT_DIR/release-lib.sh"

assert_equal() {
    local expected="$1"
    local actual="$2"
    local description="$3"
    if [[ "$actual" != "$expected" ]]; then
        echo "$description: expected '$expected', got '$actual'." >&2
        exit 1
    fi
}

assert_fails() {
    local description="$1"
    shift
    if "$@" >/dev/null 2>&1; then
        echo "$description: expected failure." >&2
        exit 1
    fi
}

assert_equal "1.2.3" "$(macos_get_bundle_version "1.2.3")" \
    "Numeric bundle version"
assert_equal "1.2.3" "$(macos_get_bundle_version "1.2.3-beta.1+build.5")" \
    "Prerelease bundle version"
assert_fails "Invalid semantic version" macos_get_bundle_version "1.2"
assert_equal "Ramag-1.2.3-macos-arm64.dmg" \
    "$(macos_get_release_asset_name "1.2.3" "arm64")" \
    "Apple Silicon release asset name"
assert_equal "Ramag-1.2.3-macos-x86_64.dmg" \
    "$(macos_get_release_asset_name "1.2.3" "x86_64")" \
    "Intel release asset name"
assert_fails "Universal release asset" \
    macos_get_release_asset_name "1.2.3" "universal"

(
    unset GITHUB_REF_TYPE GITHUB_REF_NAME GITHUB_REF
    macos_assert_tag_matches_version "1.2.3"
)
(
    export GITHUB_REF_TYPE="tag"
    export GITHUB_REF_NAME="v1.2.3"
    unset GITHUB_REF
    macos_assert_tag_matches_version "1.2.3"
)
(
    unset GITHUB_REF_TYPE GITHUB_REF_NAME
    export GITHUB_REF="refs/tags/v1.2.3-beta.1"
    macos_assert_tag_matches_version "1.2.3-beta.1"
)
# 子 shell 内的 $1 需要在运行时展开，外层必须使用单引号。
# shellcheck disable=SC2016
assert_fails "Mismatched release tag" bash -c '
    source "$1"
    export GITHUB_REF_TYPE=tag
    export GITHUB_REF_NAME=v1.2.4
    macos_assert_tag_matches_version 1.2.3
' _ "$SCRIPT_DIR/release-lib.sh"
# shellcheck disable=SC2016
assert_fails "Missing tag name" bash -c '
    source "$1"
    export GITHUB_REF_TYPE=tag
    unset GITHUB_REF_NAME GITHUB_REF
    macos_assert_tag_matches_version 1.2.3
' _ "$SCRIPT_DIR/release-lib.sh"
# shellcheck disable=SC2016
assert_fails "Empty tag ref" bash -c '
    source "$1"
    unset GITHUB_REF_TYPE GITHUB_REF_NAME
    export GITHUB_REF=refs/tags/
    macos_assert_tag_matches_version 1.2.3
' _ "$SCRIPT_DIR/release-lib.sh"

cargo_version="$(macos_get_app_version "$REPO_DIR")"
if ! macos_is_supported_semver "$cargo_version"; then
    echo "Cargo application version is invalid: $cargo_version" >&2
    exit 1
fi

echo "macOS packaging tests passed."
