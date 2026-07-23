# Windows 打包的纯逻辑回归测试；不编译应用或运行安装器。
BeforeAll {
    . (Join-Path $PSScriptRoot "..\package-windows.ps1")
    . (Join-Path $PSScriptRoot "pe-dependencies.ps1")
}

Describe "Get-VersionInfoNumber" {
    It "keeps a numeric release version" {
        Get-VersionInfoNumber -Version "1.2.3" | Should -BeExactly "1.2.3"
    }

    It "strips prerelease and build metadata" {
        Get-VersionInfoNumber -Version "1.2.3-beta.1+build.5" |
            Should -BeExactly "1.2.3"
    }

    It "accepts the largest Windows version component" {
        Get-VersionInfoNumber -Version "65535.0.1" | Should -BeExactly "65535.0.1"
    }

    It "rejects a Windows version component overflow" {
        { Get-VersionInfoNumber -Version "65536.0.1" } |
            Should -Throw "*exceeds 65535*"
    }
}

Describe "Assert-TagMatchesVersion" {
    BeforeEach {
        $script:PreviousRefType = $env:GITHUB_REF_TYPE
        $script:PreviousRefName = $env:GITHUB_REF_NAME
        $script:PreviousRef = $env:GITHUB_REF
        $env:GITHUB_REF_TYPE = $null
        $env:GITHUB_REF_NAME = $null
        $env:GITHUB_REF = $null
    }

    AfterEach {
        $env:GITHUB_REF_TYPE = $script:PreviousRefType
        $env:GITHUB_REF_NAME = $script:PreviousRefName
        $env:GITHUB_REF = $script:PreviousRef
    }

    It "allows non-tag builds" {
        { Assert-TagMatchesVersion -Version "1.2.3" } | Should -Not -Throw
    }

    It "accepts an exact tag from GitHub metadata" {
        $env:GITHUB_REF_TYPE = "tag"
        $env:GITHUB_REF_NAME = "v1.2.3"
        { Assert-TagMatchesVersion -Version "1.2.3" } | Should -Not -Throw
    }

    It "accepts an exact tag from the full ref fallback" {
        $env:GITHUB_REF = "refs/tags/v1.2.3-beta.1"
        { Assert-TagMatchesVersion -Version "1.2.3-beta.1" } | Should -Not -Throw
    }

    It "rejects a tag that differs from Cargo" {
        $env:GITHUB_REF_TYPE = "tag"
        $env:GITHUB_REF_NAME = "v1.2.4"
        { Assert-TagMatchesVersion -Version "1.2.3" } |
            Should -Throw "*does not match Cargo version v1.2.3*"
    }
}

Describe "Get-AppMetadata" {
    It "validates version and repository without invoking Cargo" {
        $MetadataJson = @{
            packages = @(
                @{
                    name = "ramag-bin"
                    version = "1.2.3-beta.1"
                    repository = "https://github.com/tools-rs/ramag"
                }
            )
        } | ConvertTo-Json -Depth 3

        $Metadata = ConvertTo-AppMetadata -MetadataJson $MetadataJson
        $Metadata.Version | Should -BeExactly "1.2.3-beta.1"
        $Metadata.Repository | Should -BeExactly "https://github.com/tools-rs/ramag"
    }

    It "rejects a repository URL that cannot produce installer links" {
        $MetadataJson = @{
            packages = @(
                @{
                    name = "ramag-bin"
                    version = "1.2.3"
                    repository = "https://example.com/tools-rs/ramag"
                }
            )
        } | ConvertTo-Json -Depth 3

        { ConvertTo-AppMetadata -MetadataJson $MetadataJson } |
            Should -Throw "*Unsupported Cargo repository URL*"
    }

    It "reads version and repository from ramag-bin metadata" -Tag "RequiresCargo" {
        $Metadata = Get-AppMetadata
        $Metadata.Version | Should -Match '^\d+\.\d+\.\d+(?:[-+].+)?$'
        $Metadata.Repository | Should -BeExactly "https://github.com/tools-rs/ramag"
    }
}

Describe "Inno Setup metadata" {
    It "receives the repository URL from the packaging script" {
        $IssContent = Get-Content `
            -LiteralPath (Join-Path $PSScriptRoot "ramag.iss") `
            -Raw

        $IssContent | Should -Match '#define MyAppURL GetEnv\("RAMAG_PACKAGE_URL"\)'
        $IssContent | Should -Not -Match 'github\.com/axemc/ramag'
    }
}

Describe "PE dependency classification" {
    BeforeAll {
        $SystemDirectory = Join-Path $TestDrive "System32"
        New-Item -ItemType Directory -Path $SystemDirectory | Out-Null
        Set-Content -LiteralPath (Join-Path $SystemDirectory "kernel32.dll") -Value "test"
    }

    It "accepts Windows API Set contracts without physical files" {
        $Dependencies = @(
            "api-ms-win-core-synch-l1-2-0.dll",
            "api-ms-win-core-winrt-error-l1-1-0.dll",
            "api-ms-win-core-winrt-l1-1-0.dll",
            "api-ms-win-shcore-scaling-l1-1-1.dll",
            "ext-ms-win-ntuser-window-l1-1-0.dll"
        )

        $Result = @(
            Get-UnpackagedPeDependencies `
                -DependencyNames $Dependencies `
                -SystemDirectory $SystemDirectory
        )
        $Result.Count | Should -Be 0
    }

    It "accepts a physical DLL from the system directory" {
        $Result = @(
            Get-UnpackagedPeDependencies `
                -DependencyNames @("kernel32.dll") `
                -SystemDirectory $SystemDirectory
        )
        $Result.Count | Should -Be 0
    }

    It "rejects a missing ordinary DLL" {
        $Result = @(
            Get-UnpackagedPeDependencies `
                -DependencyNames @("missing-runtime.dll") `
                -SystemDirectory $SystemDirectory
        )
        $Result | Should -HaveCount 1
        $Result[0] | Should -BeExactly "missing-runtime.dll"
    }

    It "does not accept a filename that only resembles an API Set contract" {
        $Result = @(
            Get-UnpackagedPeDependencies `
                -DependencyNames @("api-ms-win-core-test.dll.backup") `
                -SystemDirectory $SystemDirectory
        )
        $Result | Should -HaveCount 1
        $Result[0] | Should -BeExactly "api-ms-win-core-test.dll.backup"
    }
}
