# Finds the complete Visual Studio 18 2026 Build Tools instance used by Windows builds.
function Get-VisualStudio18Toolchain {
    $ProgramFilesX86 = [System.Environment]::GetFolderPath("ProgramFilesX86")
    $Vswhere = Join-Path $ProgramFilesX86 "Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path -LiteralPath $Vswhere -PathType Leaf)) {
        throw "vswhere.exe is required to locate Visual Studio 18 2026 Build Tools."
    }

    $JsonLines = & $Vswhere `
        -products Microsoft.VisualStudio.Product.BuildTools `
        -all `
        -prerelease `
        -version "[18.0,19.0)" `
        -format json 2>$null
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to query Visual Studio 18 2026 Build Tools."
    }

    try {
        $Instances = @(($JsonLines -join "`n") | ConvertFrom-Json)
    }
    catch {
        throw "Visual Studio 18 2026 installation data is invalid: $($_.Exception.Message)"
    }

    $Instance = @(
        $Instances |
            Where-Object {
                -not [string]::IsNullOrWhiteSpace([string]$_.installationPath) -and
                $_.isComplete -and
                $_.isLaunchable
            } |
            Sort-Object { [version]$_.installationVersion } -Descending |
            Select-Object -First 1
    )
    if ($Instance.Count -eq 0) {
        throw "A complete, launchable Visual Studio 18 2026 Build Tools installation is required."
    }

    $InstallationPath = [string]$Instance[0].installationPath
    $VcVars64 = Join-Path $InstallationPath "VC\Auxiliary\Build\vcvars64.bat"
    if (-not (Test-Path -LiteralPath $VcVars64 -PathType Leaf)) {
        throw "Visual Studio 18 2026 vcvars64.bat is missing: $VcVars64"
    }

    return [PSCustomObject]@{
        InstallationPath = $InstallationPath
        InstallationVersion = [string]$Instance[0].installationVersion
        VcVars64 = $VcVars64
    }
}
