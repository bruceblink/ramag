#!/usr/bin/env bash
# macOS 发布脚本共用的版本与标签校验逻辑。

macos_is_supported_semver() {
    local version="$1"
    [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]
}

macos_get_app_version() {
    local repo_dir="$1"
    local metadata
    local package_count
    local version

    if ! metadata="$(cd "$repo_dir" && cargo metadata --locked --no-deps --format-version 1)"; then
        echo "Failed to read Cargo metadata." >&2
        return 1
    fi
    if ! package_count="$(
        printf '%s' "$metadata" |
            jq -r '[.packages[] | select(.name == "ramag-bin")] | length'
    )"; then
        echo "Failed to parse Cargo metadata." >&2
        return 1
    fi
    if [[ "$package_count" != "1" ]]; then
        echo "Cargo metadata must contain exactly one ramag-bin package." >&2
        return 1
    fi
    if ! version="$(
        printf '%s' "$metadata" |
            jq -er '.packages[] | select(.name == "ramag-bin") | .version'
    )"; then
        echo "Failed to read the ramag-bin version from Cargo metadata." >&2
        return 1
    fi
    if ! macos_is_supported_semver "$version"; then
        echo "Unsupported Cargo version for macOS packaging: $version" >&2
        return 1
    fi
    printf '%s\n' "$version"
}

macos_get_bundle_version() {
    local version="$1"
    local bundle_version

    if ! macos_is_supported_semver "$version"; then
        echo "Unsupported Cargo version for macOS packaging: $version" >&2
        return 1
    fi
    bundle_version="${version%%[-+]*}"
    if [[ ! "$bundle_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        echo "Invalid macOS bundle version: $bundle_version" >&2
        return 1
    fi
    printf '%s\n' "$bundle_version"
}

macos_get_release_asset_name() {
    local version="$1"
    local architecture="$2"

    if ! macos_is_supported_semver "$version"; then
        echo "Unsupported Cargo version for macOS packaging: $version" >&2
        return 1
    fi
    case "$architecture" in
        arm64|x86_64) ;;
        *)
            echo "Unsupported macOS release architecture: $architecture" >&2
            return 1
            ;;
    esac
    printf 'Ramag-%s-macos-%s.dmg\n' "$version" "$architecture"
}

macos_assert_tag_matches_version() {
    local version="$1"
    local tag=""
    local expected_tag="v$version"

    if [[ "${GITHUB_REF_TYPE:-}" == "tag" ]]; then
        tag="${GITHUB_REF_NAME:-}"
        if [[ -z "$tag" ]]; then
            echo "GITHUB_REF_TYPE is tag, but GITHUB_REF_NAME is empty." >&2
            return 1
        fi
    elif [[ "${GITHUB_REF:-}" == refs/tags/* ]]; then
        tag="${GITHUB_REF#refs/tags/}"
        if [[ -z "$tag" ]]; then
            echo "GITHUB_REF contains an empty tag name." >&2
            return 1
        fi
    fi
    if [[ -z "$tag" ]]; then
        return 0
    fi
    if [[ "$tag" != "$expected_tag" ]]; then
        echo "Release tag $tag does not match Cargo version $expected_tag." >&2
        return 1
    fi
}
