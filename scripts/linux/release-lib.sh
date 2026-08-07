#!/usr/bin/env bash
# Linux 发布脚本共用的版本、文件名与标签校验逻辑。

linux_is_supported_semver() {
    local version="$1"
    [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]
}

linux_get_app_version() {
    local repo_dir="$1"
    local metadata package_count version

    metadata="$(cd "$repo_dir" && cargo metadata --locked --no-deps --format-version 1)" || {
        echo "Failed to read Cargo metadata." >&2
        return 1
    }
    package_count="$(printf '%s' "$metadata" | jq -r \
        '[.packages[] | select(.name == "ramag-bin")] | length')" || return 1
    if [[ "$package_count" != "1" ]]; then
        echo "Cargo metadata must contain exactly one ramag-bin package." >&2
        return 1
    fi
    version="$(printf '%s' "$metadata" | jq -er \
        '.packages[] | select(.name == "ramag-bin") | .version')" || return 1
    if ! linux_is_supported_semver "$version"; then
        echo "Unsupported Cargo version for Linux packaging: $version" >&2
        return 1
    fi
    printf '%s\n' "$version"
}

linux_get_deb_asset_name() {
    local version="$1"
    linux_is_supported_semver "$version" || return 1
    printf 'Ramag-%s-linux-amd64.deb\n' "$version"
}

linux_get_appimage_asset_name() {
    local version="$1"
    linux_is_supported_semver "$version" || return 1
    printf 'Ramag-%s-linux-x86_64.AppImage\n' "$version"
}

linux_assert_tag_matches_version() {
    local version="$1"
    local tag=""
    local expected_tag="v$version"

    if [[ "${GITHUB_REF_TYPE:-}" == "tag" ]]; then
        tag="${GITHUB_REF_NAME:-}"
        [[ -n "$tag" ]] || {
            echo "GITHUB_REF_TYPE is tag, but GITHUB_REF_NAME is empty." >&2
            return 1
        }
    elif [[ "${GITHUB_REF:-}" == refs/tags/* ]]; then
        tag="${GITHUB_REF#refs/tags/}"
        [[ -n "$tag" ]] || {
            echo "GITHUB_REF contains an empty tag name." >&2
            return 1
        }
    fi
    [[ -z "$tag" || "$tag" == "$expected_tag" ]] || {
        echo "Release tag $tag does not match Cargo version $expected_tag." >&2
        return 1
    }
}
