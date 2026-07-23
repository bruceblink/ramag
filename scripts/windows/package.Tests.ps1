# Windows 打包的纯逻辑回归测试；不编译应用或运行安装器。
BeforeAll {
    . (Join-Path $PSScriptRoot "..\package-windows.ps1")
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
