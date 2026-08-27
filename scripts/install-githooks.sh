#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
hook_file="$repo_root/.githooks/pre-commit"

if [[ ! -f "$hook_file" ]]; then
    printf 'Git hook is missing: %s\n' "$hook_file" >&2
    exit 1
fi

git -C "$repo_root" config --local core.hooksPath .githooks
printf '%s\n' 'Git hooks enabled: .githooks'
