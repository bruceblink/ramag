# Finds a complete Visual Studio 18 2026 installation with a usable MSVC toolset.
function Get-VisualStudio18Toolchain {
    $ProgramFilesX86 = [System.Environment]::GetFolderPath("ProgramFilesX86")
    $Vswhere = Join-Path $ProgramFilesX86 "Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path -LiteralPath $Vswhere -PathType Leaf)) {
        throw "vswhere.exe is required to locate Visual Studio 18 2026."
    }

    $JsonLines = & $Vswhere `
        -products '*' `
        -all `
        -prerelease `
        -version "[18.0,19.0)" `
        -format json 2>$null
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to query Visual Studio 18 2026 installations."
    }

    try {
        $Instances = @(($JsonLines -join "`n") | ConvertFrom-Json)
    }
    catch {
        throw "Visual Studio 18 2026 installation data is invalid: $($_.Exception.Message)"
    }

    $Instances = @(
        $Instances |
            Where-Object {
                -not [string]::IsNullOrWhiteSpace([string]$_.installationPath) -and
                $_.isComplete -and
                $_.isLaunchable
            } |
            Sort-Object { [version]$_.installationVersion } -Descending
    )

    foreach ($Instance in $Instances) {
        $InstallationPath = [string]$Instance.installationPath
        $VcVarsAll = Join-Path $InstallationPath "VC\Auxiliary\Build\vcvarsall.bat"
        $MsvcRoot = Join-Path $InstallationPath "VC\Tools\MSVC"
        if (-not (Test-Path -LiteralPath $VcVarsAll -PathType Leaf) -or
            -not (Test-Path -LiteralPath $MsvcRoot -PathType Container)) {
            continue
        }

        $Toolset = Get-ChildItem -LiteralPath $MsvcRoot -Directory |
            Sort-Object { [version]$_.Name } -Descending |
            Where-Object {
                Test-Path -LiteralPath (Join-Path $_.FullName "bin\Hostx64\x64\cl.exe") -PathType Leaf
            } |
            Select-Object -First 1
        if ($null -eq $Toolset) {
            continue
        }

        # VS 18 may contain a partially installed preview toolset. Pin the
        # newest toolset whose compiler is actually present instead of letting
        # vcvarsall select the incomplete default.
        $ToolsetVersion = ([regex]::Match($Toolset.Name, '^\d+\.\d+')).Value
        return [PSCustomObject]@{
            InstallationPath = $InstallationPath
            InstallationVersion = [string]$Instance.installationVersion
            VcVars64 = $VcVarsAll
            VcVarsArguments = @("x64", "-vcvars_ver=$ToolsetVersion")
            ToolsetVersion = [string]$Toolset.Name
        }
    }

    throw "A complete, launchable Visual Studio 18 2026 installation with a usable x64 MSVC toolset is required."
}
