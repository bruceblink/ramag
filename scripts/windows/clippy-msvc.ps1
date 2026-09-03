# Runs the workspace Clippy check inside the selected Visual Studio MSVC environment.
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoDir = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$ToolchainHelper = Join-Path $PSScriptRoot "msvc-toolchain.ps1"
if (-not (Test-Path -LiteralPath $ToolchainHelper -PathType Leaf)) {
    throw "MSVC toolchain helper is missing: $ToolchainHelper"
}

. $ToolchainHelper
Set-Location $RepoDir
$VisualStudio = Get-VisualStudio18Toolchain
Write-Host "Using Visual Studio 18 2026 MSVC: $($VisualStudio.InstallationPath) ($($VisualStudio.InstallationVersion)); toolset $($VisualStudio.ToolsetVersion)"

$Target = "x86_64-pc-windows-msvc"
$TargetEnvSuffix = $Target.Replace("-", "_")
$CompilerEnvironmentNames = @(
    "CC",
    "CXX",
    "AR",
    "HOST_CC",
    "HOST_CXX",
    "HOST_AR",
    "CC_$Target",
    "CC_$TargetEnvSuffix",
    "CXX_$Target",
    "CXX_$TargetEnvSuffix",
    "AR_$Target",
    "AR_$TargetEnvSuffix",
    "CFLAGS",
    "CXXFLAGS",
    "ARFLAGS",
    "CFLAGS_$Target",
    "CFLAGS_$TargetEnvSuffix",
    "CXXFLAGS_$Target",
    "CXXFLAGS_$TargetEnvSuffix",
    "ARFLAGS_$Target",
    "ARFLAGS_$TargetEnvSuffix"
)
$SavedCompilerEnvironment = @{}
foreach ($Name in $CompilerEnvironmentNames) {
    $Variable = Get-Item -Path "Env:$Name" -ErrorAction SilentlyContinue
    if ($null -ne $Variable) {
        $SavedCompilerEnvironment[$Name] = $Variable.Value
    }
    Remove-Item -Path "Env:$Name" -ErrorAction SilentlyContinue
}

$ClippyExitCode = 1
try {
    # Git Bash can prepend its GNU linker after the hook starts; initialize MSVC
    # in the same cmd.exe process that launches Cargo so the selected linker wins.
    $VcVarsArguments = $VisualStudio.VcVarsArguments -join " "
    $Command = 'call "{0}" {1} >nul && set "CMAKE_GENERATOR=NMake Makefiles" && set "CMAKE_GENERATOR_PLATFORM=" && set "CMAKE_GENERATOR_INSTANCE=" && set "CMAKE_GENERATOR_TOOLSET=" && where cl.exe && where link.exe && cargo clippy-all' -f `
        $VisualStudio.VcVars64,
        $VcVarsArguments
    & cmd.exe /d /s /c $Command
    $ClippyExitCode = $LASTEXITCODE
}
finally {
    foreach ($Name in $CompilerEnvironmentNames) {
        if ($SavedCompilerEnvironment.ContainsKey($Name)) {
            Set-Item -Path "Env:$Name" -Value $SavedCompilerEnvironment[$Name]
        }
        else {
            Remove-Item -Path "Env:$Name" -ErrorAction SilentlyContinue
        }
    }
}

exit $ClippyExitCode
