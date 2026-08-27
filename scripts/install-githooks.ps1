Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoRoot = (& git rev-parse --show-toplevel 2>$null).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($RepoRoot)) {
    throw "The current directory is not inside a Git repository."
}

$HookFile = Join-Path $RepoRoot ".githooks\pre-commit"
if (-not (Test-Path -LiteralPath $HookFile -PathType Leaf)) {
    throw "Git hook is missing: $HookFile"
}

& git -C $RepoRoot config --local core.hooksPath .githooks
if ($LASTEXITCODE -ne 0) {
    throw "Failed to configure the repository Git hooks path."
}

Write-Host "Git hooks enabled: .githooks"
