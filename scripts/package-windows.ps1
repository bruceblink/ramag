# Windows x64 安装包发布入口；CI 默认启用冒烟测试，本地需显式传入 -SmokeTest。
param(
    [switch]$SmokeTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$Target = "x86_64-pc-windows-msvc"
$RepoDir = Split-Path -Parent $PSScriptRoot
$BuildScript = Join-Path $PSScriptRoot "build-windows.ps1"
$IssScript = Join-Path $PSScriptRoot "windows\ramag.iss"
$SmokeTestScript = Join-Path $PSScriptRoot "windows\test-installer.ps1"
$Exe = Join-Path $RepoDir "target\$Target\release\ramag.exe"
$DistDir = Join-Path $RepoDir "target\windows-dist"
$WorkDir = Join-Path $RepoDir "target\windows-package"

function Find-Iscc {
    $Command = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($null -ne $Command) {
        return $Command.Source
    }

    $Candidates = @(
        (Join-Path ([System.Environment]::GetFolderPath("ProgramFilesX86")) "Inno Setup 6\ISCC.exe"),
        (Join-Path ([System.Environment]::GetFolderPath("ProgramFiles")) "Inno Setup 6\ISCC.exe")
    )
    return $Candidates |
        Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
        Select-Object -First 1
}

function ConvertTo-AppMetadata {
    param(
        [Parameter(Mandatory = $true)]
        [string]$MetadataJson
    )

    try {
        $Metadata = $MetadataJson | ConvertFrom-Json
    }
    catch {
        throw "Failed to parse Cargo metadata: $($_.Exception.Message)"
    }

    $Packages = @($Metadata.packages | Where-Object { $_.name -eq "ramag-bin" })
    if ($Packages.Count -ne 1) {
        throw "Cargo metadata must contain exactly one ramag-bin package."
    }
    $Version = [string]$Packages[0].version
    if ($Version -notmatch '^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$') {
        throw "Unsupported Cargo version for Windows packaging: $Version"
    }
    $Repository = [string]$Packages[0].repository
    if ($Repository -notmatch '^https://github\.com/[0-9A-Za-z_.-]+/[0-9A-Za-z_.-]+$') {
        throw "Unsupported Cargo repository URL for Windows packaging: $Repository"
    }
    return [PSCustomObject]@{
        Version = $Version
        Repository = $Repository
    }
}

function Get-AppMetadata {
    $MetadataJson = (& cargo metadata --locked --no-deps --format-version 1) -join "`n"
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to read Cargo metadata."
    }
    return ConvertTo-AppMetadata -MetadataJson $MetadataJson
}

function Get-VersionInfoNumber {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Version
    )

    $CoreVersion = ($Version -split '[-+]', 2)[0]
    $Parts = @($CoreVersion -split '\.')
    foreach ($Part in $Parts) {
        if ([uint32]$Part -gt 65535) {
            throw "Windows version component exceeds 65535: $Version"
        }
    }
    return $CoreVersion
}

function Assert-TagMatchesVersion {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Version
    )

    $Tag = $null
    if ($env:GITHUB_REF_TYPE -eq "tag") {
        $Tag = $env:GITHUB_REF_NAME
    }
    elseif ($env:GITHUB_REF -like "refs/tags/*") {
        $Tag = $env:GITHUB_REF.Substring("refs/tags/".Length)
    }
    if ($null -eq $Tag) {
        return
    }

    $ExpectedTag = "v$Version"
    if ($Tag -cne $ExpectedTag) {
        throw "Release tag $Tag does not match Cargo version $ExpectedTag."
    }
}

function Assert-File {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [string]$Description
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Description is missing: $Path"
    }
    if ((Get-Item -LiteralPath $Path).Length -eq 0) {
        throw "$Description is empty: $Path"
    }
}

function Invoke-WindowsPackage {
    if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
        throw "This script must run on Windows. Windows release packages are built by GitHub Actions."
    }

    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw "cargo not found. Install Rust with rustup before packaging."
    }
    if (-not (Test-Path -LiteralPath $BuildScript -PathType Leaf)) {
        throw "Windows build script is missing: $BuildScript"
    }
    if (-not (Test-Path -LiteralPath $IssScript -PathType Leaf)) {
        throw "Inno Setup script is missing: $IssScript"
    }
    if ($SmokeTest -and -not (Test-Path -LiteralPath $SmokeTestScript -PathType Leaf)) {
        throw "Installer smoke-test script is missing: $SmokeTestScript"
    }

    Set-Location $RepoDir
    $AppMetadata = Get-AppMetadata
    $Version = $AppMetadata.Version
    $AppUrl = $AppMetadata.Repository
    $VersionInfoNumber = Get-VersionInfoNumber -Version $Version
    Assert-TagMatchesVersion -Version $Version

    & $BuildScript -Release
    Assert-File -Path $Exe -Description "Release executable"
    $ExeVersionInfo = [System.Diagnostics.FileVersionInfo]::GetVersionInfo($Exe)
    if ($ExeVersionInfo.ProductVersion -ne $Version) {
        throw "Executable version $($ExeVersionInfo.ProductVersion) does not match Cargo version $Version."
    }

    $Iscc = Find-Iscc
    if ([string]::IsNullOrWhiteSpace($Iscc)) {
        throw "ISCC.exe not found. Install Inno Setup 6 before packaging."
    }
    $IsccVersion = [System.Diagnostics.FileVersionInfo]::GetVersionInfo($Iscc).ProductVersion
    Write-Host "Using Inno Setup: $Iscc ($IsccVersion)"

    if (Test-Path -LiteralPath $DistDir) {
        Remove-Item -LiteralPath $DistDir -Recurse -Force
    }
    if (Test-Path -LiteralPath $WorkDir) {
        Remove-Item -LiteralPath $WorkDir -Recurse -Force
    }
    New-Item -ItemType Directory -Path $DistDir -Force | Out-Null
    New-Item -ItemType Directory -Path $WorkDir -Force | Out-Null

    $SetupBaseName = "Ramag-$Version-windows-x64-setup"
    $SetupPath = Join-Path $DistDir "$SetupBaseName.exe"
    $PreviousPackageExe = $env:RAMAG_PACKAGE_EXE
    $PreviousPackageVersion = $env:RAMAG_PACKAGE_VERSION
    $PreviousPackageVersionInfo = $env:RAMAG_PACKAGE_VERSION_INFO
    $PreviousPackageUrl = $env:RAMAG_PACKAGE_URL
    try {
        $env:RAMAG_PACKAGE_EXE = $Exe
        $env:RAMAG_PACKAGE_VERSION = $Version
        $env:RAMAG_PACKAGE_VERSION_INFO = $VersionInfoNumber
        $env:RAMAG_PACKAGE_URL = $AppUrl
        & $Iscc "/Qp" "/O$DistDir" "/F$SetupBaseName" $IssScript
        if ($LASTEXITCODE -ne 0) {
            throw "Inno Setup compilation failed with exit code $LASTEXITCODE."
        }
    }
    finally {
        $env:RAMAG_PACKAGE_EXE = $PreviousPackageExe
        $env:RAMAG_PACKAGE_VERSION = $PreviousPackageVersion
        $env:RAMAG_PACKAGE_VERSION_INFO = $PreviousPackageVersionInfo
        $env:RAMAG_PACKAGE_URL = $PreviousPackageUrl
    }
    Assert-File -Path $SetupPath -Description "Windows installer"

    if ($SmokeTest) {
        & $SmokeTestScript `
            -SetupPath $SetupPath `
            -ExpectedVersion $Version `
            -WorkDir $WorkDir `
            -AppId "com.axemc.ramag"
    }

    $HashPath = Join-Path $DistDir "SHA256SUMS.txt"
    $Assets = @(Get-Item -LiteralPath $SetupPath)
    $HashLines = @(
        $Assets | ForEach-Object {
            $Hash = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
            "$Hash  $($_.Name)"
        }
    )
    [System.IO.File]::WriteAllLines(
        $HashPath,
        $HashLines,
        [System.Text.UTF8Encoding]::new($false)
    )
    Assert-File -Path $HashPath -Description "SHA-256 checksum file"

    foreach ($Asset in $Assets) {
        $ExpectedHashLine = $HashLines | Where-Object { $_ -like "*  $($Asset.Name)" }
        $ActualHash = (Get-FileHash -LiteralPath $Asset.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($ExpectedHashLine -cne "$ActualHash  $($Asset.Name)") {
            throw "SHA-256 verification failed for $($Asset.Name)."
        }
    }

    Write-Host "Windows package completed: $DistDir"
    Get-ChildItem -LiteralPath $DistDir -File | ForEach-Object {
        Write-Host "  $($_.Name) ($($_.Length) bytes)"
    }
}

if ($MyInvocation.InvocationName -ne ".") {
    Invoke-WindowsPackage
}
