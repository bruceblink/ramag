# PE 依赖分类纯函数；API Set 是加载器契约，不要求 System32 中存在同名文件。
function Test-WindowsApiSetContract {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Name
    )

    return $Name -match '^(api-ms-win-|ext-ms-win-)[A-Z0-9._+-]+\.dll$'
}

function Get-UnpackagedPeDependencies {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$DependencyNames,
        [Parameter(Mandatory = $true)]
        [string]$SystemDirectory
    )

    if (-not (Test-Path -LiteralPath $SystemDirectory -PathType Container)) {
        throw "Windows system directory is missing: $SystemDirectory"
    }

    return @(
        $DependencyNames | Where-Object {
            -not (Test-WindowsApiSetContract -Name $_) -and
            -not (Test-Path -LiteralPath (Join-Path $SystemDirectory $_) -PathType Leaf)
        }
    )
}
