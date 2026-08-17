#!/usr/bin/env bash
set -euo pipefail

readonly log_pattern='(?<![[:alnum:]_:])(?:tracing::)?(?:error|warn|info|debug|trace)!\(\s*+(?!operation\s*(?:=|,))'
readonly error_context_pattern='(?s)(?<![[:alnum:]_:])(?:tracing::)?error!\((?!(?:(?!\);).)*\b(?:error|reason|panic)\s*[=,])(?:(?!\);).)*\);'

if violations=$(rg -n -o -U --pcre2 --glob '*.rs' "$log_pattern" crates); then
    printf '%s\n' 'tracing log fields must start with operation'
    printf '%s\n' "$violations"
    exit 1
else
    status=$?
fi

if (( status != 1 )); then
    exit "$status"
fi

if violations=$(rg -n -o -U --pcre2 --glob '*.rs' "$error_context_pattern" crates); then
    printf '%s\n' 'error logs must include error, reason, or panic context'
    printf '%s\n' "$violations"
    exit 1
else
    status=$?
fi

if (( status != 1 )); then
    exit "$status"
fi
