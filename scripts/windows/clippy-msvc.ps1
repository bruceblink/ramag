# Runs the workspace Clippy check inside the selected Visual Studio MSVC environment.
[CmdletBinding()]
param()

& (Join-Path $PSScriptRoot "cargo-msvc.ps1") clippy-all
exit $LASTEXITCODE
