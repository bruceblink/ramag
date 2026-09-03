# Checks Rust source files under crates and matches scripts/check-source-size.sh.
[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [string]$RepoRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$MaxLines = 600
if ([string]::IsNullOrWhiteSpace($RepoRoot)) {
    $RepoRoot = Join-Path $PSScriptRoot "..\.."
}
$RepoRoot = [System.IO.Path]::GetFullPath($RepoRoot).TrimEnd('\', '/')
$CratesRoot = Join-Path $RepoRoot "crates"

if (-not (Test-Path -LiteralPath $CratesRoot -PathType Container)) {
    [Console]::Error.WriteLine("Rust source root is missing: $CratesRoot")
    exit 1
}

$Failed = $false
$SourceFiles = @(Get-ChildItem -LiteralPath $CratesRoot -Recurse -File -Filter "*.rs" | Sort-Object -Property FullName)
foreach ($SourceFile in $SourceFiles) {
    $LineCount = 0
    $Reader = [System.IO.StreamReader]::new($SourceFile.FullName)
    try {
        while ($null -ne $Reader.ReadLine()) {
            $LineCount++
        }
    }
    finally {
        $Reader.Dispose()
    }

    if ($LineCount -gt $MaxLines) {
        $RelativePath = $SourceFile.FullName.Substring($RepoRoot.Length).TrimStart('\', '/')
        $RelativePath = $RelativePath.Replace('\', '/')
        $Diagnostic = "{0}: {1} lines (max {2})" -f @($RelativePath, $LineCount, $MaxLines)
        [Console]::Error.WriteLine($Diagnostic)
        $Failed = $true
    }
}

if ($Failed) {
    exit 1
}
exit 0
