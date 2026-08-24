# Windows 原生 x64 构建；Release 由 Windows SDK fxc.exe 预编译 GPUI 着色器。
param(
    [switch]$Release
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$Target = "x86_64-pc-windows-msvc"
$BuildProfile = if ($Release) { "release" } else { "debug" }
$RepoDir = Split-Path -Parent $PSScriptRoot
$DependencyHelper = Join-Path $PSScriptRoot "windows\pe-dependencies.ps1"

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw "This script must run on Windows. Use scripts/build-windows-local.sh for macOS cross-checks."
}
if (-not (Test-Path -LiteralPath $DependencyHelper -PathType Leaf)) {
    throw "PE dependency helper is missing: $DependencyHelper"
}
. $DependencyHelper

Set-Location $RepoDir

function Find-Fxc {
    $Command = Get-Command fxc.exe -ErrorAction SilentlyContinue
    if ($null -ne $Command) {
        return $Command.Source
    }

    $RegistryPath = "HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots"
    $InstalledRoots = Get-ItemProperty -LiteralPath $RegistryPath -ErrorAction SilentlyContinue
    if ($null -eq $InstalledRoots) {
        return $null
    }
    $KitsRootProperty = $InstalledRoots.PSObject.Properties["KitsRoot10"]
    if ($null -eq $KitsRootProperty) {
        return $null
    }
    $KitsRoot = $KitsRootProperty.Value
    if ([string]::IsNullOrWhiteSpace($KitsRoot)) {
        return $null
    }

    $BinDir = Join-Path $KitsRoot "bin"
    if (-not (Test-Path -LiteralPath $BinDir -PathType Container)) {
        return $null
    }
    return Get-ChildItem -LiteralPath $BinDir -Directory |
        Where-Object { $_.Name -match '^\d+(\.\d+){1,3}$' } |
        Sort-Object { [version]$_.Name } -Descending |
        ForEach-Object { Join-Path $_.FullName "x64\fxc.exe" } |
        Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
        Select-Object -First 1
}

function Find-Dumpbin {
    $Command = Get-Command dumpbin.exe -ErrorAction SilentlyContinue
    if ($null -ne $Command) {
        return $Command.Source
    }

    $ProgramFilesX86 = [System.Environment]::GetFolderPath("ProgramFilesX86")
    $Vswhere = Join-Path $ProgramFilesX86 "Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path -LiteralPath $Vswhere -PathType Leaf)) {
        return $null
    }
    $DumpbinMatches = & $Vswhere `
        -latest `
        -products * `
        -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
        -find "VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe"
    if ($LASTEXITCODE -ne 0) {
        return $null
    }
    return $DumpbinMatches |
        Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
        Select-Object -First 1
}

function Assert-PeTarget {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [bool]$Gui
    )

    $Stream = [System.IO.File]::OpenRead($Path)
    $Reader = [System.IO.BinaryReader]::new($Stream)
    try {
        if ($Reader.ReadUInt16() -ne 0x5A4D) {
            throw "Output is not a PE executable: $Path"
        }
        $Stream.Seek(0x3C, [System.IO.SeekOrigin]::Begin) | Out-Null
        $PeOffset = $Reader.ReadInt32()
        $Stream.Seek($PeOffset, [System.IO.SeekOrigin]::Begin) | Out-Null
        if ($Reader.ReadUInt32() -ne 0x00004550) {
            throw "Output has an invalid PE signature: $Path"
        }
        if ($Reader.ReadUInt16() -ne 0x8664) {
            throw "Output is not an x64 executable: $Path"
        }
        $OptionalHeader = $PeOffset + 24
        $Stream.Seek($OptionalHeader, [System.IO.SeekOrigin]::Begin) | Out-Null
        if ($Reader.ReadUInt16() -ne 0x020B) {
            throw "Output is not a PE32+ executable: $Path"
        }
        $Stream.Seek($OptionalHeader + 68, [System.IO.SeekOrigin]::Begin) | Out-Null
        $Subsystem = $Reader.ReadUInt16()
        $ExpectedSubsystem = if ($Gui) { 2 } else { 3 }
        if ($Subsystem -ne $ExpectedSubsystem) {
            throw "Unexpected PE subsystem $Subsystem (expected $ExpectedSubsystem): $Path"
        }
    }
    finally {
        $Reader.Dispose()
        $Stream.Dispose()
    }
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo not found. Install Rust with rustup before building."
}

if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) {
    throw "rustup not found. Install Rust from https://rustup.rs before building."
}

$InstalledTargets = & rustup target list --installed
if ($LASTEXITCODE -ne 0) {
    throw "Failed to query installed Rust targets."
}
if ($InstalledTargets -notcontains $Target) {
    & rustup target add $Target
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to install Rust target $Target."
    }
}

if ($Release) {
    $Fxc = Find-Fxc
    if ([string]::IsNullOrWhiteSpace($Fxc)) {
        throw "fxc.exe not found. Install the Windows 10/11 SDK before building a release."
    }
    $env:GPUI_FXC_PATH = $Fxc
    Write-Host "Using HLSL compiler: $Fxc"
}

$CargoArgs = @("build", "--locked", "--target", $Target, "-p", "ramag-bin")
if ($Release) {
    $CargoArgs += "--release"
}

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

try {
    # tree-sitter and other C build scripts must use the MSVC ABI for this target;
    # inherited GNU compiler variables would produce MinGW objects for link.exe.
    & cargo @CargoArgs
    $CargoExitCode = $LASTEXITCODE
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

if ($CargoExitCode -ne 0) {
    throw "Windows $BuildProfile build failed. Install Visual Studio C++ Build Tools and the Windows 10/11 SDK, then retry."
}

$Exe = Join-Path $RepoDir "target\$Target\$BuildProfile\ramag.exe"
if (-not (Test-Path -LiteralPath $Exe -PathType Leaf)) {
    throw "Build finished without the expected executable: $Exe"
}

$VersionInfo = [System.Diagnostics.FileVersionInfo]::GetVersionInfo($Exe)
if ($VersionInfo.ProductName -ne "Ramag") {
    throw "The executable is missing the expected Windows version resource: $Exe"
}
Assert-PeTarget -Path $Exe -Gui $Release.IsPresent

$Dumpbin = Find-Dumpbin
if ([string]::IsNullOrWhiteSpace($Dumpbin)) {
    throw "dumpbin.exe not found. Repair the Visual Studio C++ Build Tools installation."
}
$DumpbinVersion = [System.Diagnostics.FileVersionInfo]::GetVersionInfo($Dumpbin).ProductVersion
Write-Host "Using PE inspector: $Dumpbin ($DumpbinVersion)"
$Dependencies = (& $Dumpbin /nologo /dependents $Exe) -join "`n"
if ($LASTEXITCODE -ne 0) {
    throw "Failed to inspect executable dependencies with dumpbin.exe."
}
$DependencyNames = @(
    [regex]::Matches(
        $Dependencies,
        '(?im)^\s*([A-Z0-9._+-]+\.dll)\s*$'
    ) |
        ForEach-Object { $_.Groups[1].Value } |
        Sort-Object -Unique
)
if ($DependencyNames.Count -eq 0) {
    throw "dumpbin.exe returned no PE dependencies for $Exe."
}
Write-Host "PE dependencies: $($DependencyNames -join ', ')"

if ($DependencyNames -match '^(VCRUNTIME|MSVCP|api-ms-win-crt-)[^\s]*\.dll$' -or
    $DependencyNames -contains 'ucrtbase.dll') {
    throw "The executable depends on the dynamic MSVC/UCRT runtime; the release build check failed."
}

$SystemDirectory = [System.Environment]::SystemDirectory
$NonSystemDependencies = @(
    Get-UnpackagedPeDependencies `
        -DependencyNames $DependencyNames `
        -SystemDirectory $SystemDirectory
)
if ($NonSystemDependencies.Count -gt 0) {
    throw "The executable has unpackaged non-system dependencies: $($NonSystemDependencies -join ', ')"
}

$Size = (Get-Item -LiteralPath $Exe).Length
Write-Host "Windows $BuildProfile build completed: $Exe ($Size bytes)"
