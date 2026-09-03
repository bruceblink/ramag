# Runs a Cargo command inside the selected Visual Studio MSVC environment.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$CargoCommand,

    [Parameter(Position = 1, ValueFromRemainingArguments = $true)]
    [string[]]$CargoArguments = @()
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoDir = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$ToolchainHelper = Join-Path $PSScriptRoot "msvc-toolchain.ps1"
$CMakeToolchainFile = Join-Path $PSScriptRoot "msvc-static-crt.cmake"
if (-not (Test-Path -LiteralPath $ToolchainHelper -PathType Leaf)) {
    throw "MSVC toolchain helper is missing: $ToolchainHelper"
}
if (-not (Test-Path -LiteralPath $CMakeToolchainFile -PathType Leaf)) {
    throw "CMake MSVC runtime toolchain file is missing: $CMakeToolchainFile"
}

. $ToolchainHelper
$VisualStudio = Get-VisualStudio18Toolchain
Write-Host "Using Visual Studio 18 2026 MSVC: $($VisualStudio.InstallationPath) ($($VisualStudio.InstallationVersion)); toolset $($VisualStudio.ToolsetVersion)"

$PreviousLocation = Get-Location
$SavedEnvironment = @{}
foreach ($Variable in Get-ChildItem Env:) {
    $SavedEnvironment[$Variable.Name] = $Variable.Value
}

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
foreach ($Name in $CompilerEnvironmentNames) {
    Remove-Item -Path "Env:$Name" -ErrorAction SilentlyContinue
}

$CargoExitCode = 1
try {
    Set-Location -LiteralPath $RepoDir

    # Import vcvarsall output into this process so cc-rs and Cargo see the same
    # MSVC tools, even when the caller started from PowerShell or Git Bash.
    $VcVarsArguments = $VisualStudio.VcVarsArguments -join " "
    $CMakeToolchainPath = [System.IO.Path]::GetFullPath($CMakeToolchainFile)
    $Command = 'call "{0}" {1} >nul && set "CMAKE_GENERATOR=NMake Makefiles" && set "CMAKE_GENERATOR_PLATFORM=" && set "CMAKE_GENERATOR_INSTANCE=" && set "CMAKE_GENERATOR_TOOLSET=" && set "CMAKE_TOOLCHAIN_FILE={2}" && set' -f `
        $VisualStudio.VcVars64,
        $VcVarsArguments,
        $CMakeToolchainPath
    $EnvironmentLines = @(& cmd.exe /d /s /c $Command)
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to initialize the Visual Studio MSVC environment."
    }

    foreach ($Line in $EnvironmentLines) {
        $Separator = $Line.IndexOf("=")
        if ($Separator -le 0) {
            continue
        }
        $Name = $Line.Substring(0, $Separator)
        $Value = $Line.Substring($Separator + 1)
        Set-Item -Path "Env:$Name" -Value $Value
    }

    $Compiler = Get-Command cl.exe -ErrorAction Stop
    $Linker = Get-Command link.exe -ErrorAction Stop
    Write-Host "MSVC compiler: $($Compiler.Source)"
    Write-Host "MSVC linker: $($Linker.Source)"

    & cargo $CargoCommand @CargoArguments
    $CargoExitCode = $LASTEXITCODE
}
finally {
    foreach ($Variable in @(Get-ChildItem Env:)) {
        if (-not $SavedEnvironment.ContainsKey($Variable.Name)) {
            Remove-Item -Path "Env:$($Variable.Name)" -ErrorAction SilentlyContinue
        }
    }
    foreach ($Name in $SavedEnvironment.Keys) {
        Set-Item -Path "Env:$Name" -Value $SavedEnvironment[$Name]
    }
    Set-Location -LiteralPath $PreviousLocation.Path
}

exit $CargoExitCode
