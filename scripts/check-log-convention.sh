#!/usr/bin/env bash
set -euo pipefail

readonly log_pattern='(?<![[:alnum:]_:])(?:tracing::)?(?:error|warn|info|debug|trace)!\(\s*+(?!operation\s*(?:=|,))'
readonly error_context_pattern='(?s)(?<![[:alnum:]_:])(?:tracing::)?error!\((?!(?:(?!\);).)*\b(?:error|reason|panic)\s*[=,])(?:(?!\);).)*\);'

search_rust_logs() {
    local pattern=$1

    if command -v rg >/dev/null 2>&1; then
        rg -n -o -U --pcre2 --glob '*.rs' "$pattern" crates
        return
    fi

    # Perl provides the PCRE features used above on minimal macOS/Linux images.
    local output=''
    local file matches
    while IFS= read -r -d '' file; do
        matches=$(PATTERN="$pattern" perl -0777 -ne '
            my $pattern = $ENV{PATTERN};
            while (/$pattern/g) {
                my $line = 1 + substr($_, 0, $-[0]) =~ tr/\n//;
                my $match = $&;
                $match =~ s/\n/\\n/g;
                print "$ARGV:$line:$match\n";
            }
        ' "$file")
        if [[ -n "$matches" ]]; then
            output+="$matches"$'\n'
        fi
    done < <(find crates -type f -name '*.rs' -print0)

    if [[ -n "$output" ]]; then
        printf '%s' "$output"
        return 0
    fi
    return 1
}

if violations=$(search_rust_logs "$log_pattern"); then
    printf '%s\n' 'tracing log fields must start with operation'
    printf '%s\n' "$violations"
    exit 1
else
    status=$?
fi

if (( status != 1 )); then
    exit "$status"
fi

if violations=$(search_rust_logs "$error_context_pattern"); then
    printf '%s\n' 'error logs must include error, reason, or panic context'
    printf '%s\n' "$violations"
    exit 1
else
    status=$?
fi

if (( status != 1 )); then
    exit "$status"
fi
