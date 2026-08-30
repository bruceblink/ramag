[CmdletBinding()]
param(
    [ValidateSet("up", "status", "seed", "test", "down", "clean")]
    [string]$Command = "status"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$ScriptDirectory = Split-Path -Parent $MyInvocation.MyCommand.Path
$ComposeFile = Join-Path $ScriptDirectory "compose.yaml"
$FixtureFile = Join-Path $ScriptDirectory "fixtures\messages.txt"
$ProjectName = "ramag-kafka-test"
$ContainerName = "ramag-kafka-test"
$BootstrapServers = "127.0.0.1:19092"
$TopicName = "ramag.integration.messages"

function Write-TestLog {
    param([Parameter(Mandatory = $true)][string]$Message)

    Write-Host "[kafka-test] $Message"
}

function Invoke-Compose {
    param([Parameter(Mandatory = $true)][string[]]$ComposeArguments)

    & docker compose --project-name $ProjectName --file $ComposeFile @ComposeArguments
    if ($LASTEXITCODE -ne 0) {
        throw "docker compose failed with exit code $LASTEXITCODE"
    }
}

function Get-ComposeOutput {
    param([Parameter(Mandatory = $true)][string[]]$ComposeArguments)

    $stderrFile = [System.IO.Path]::GetTempFileName()
    $previousErrorActionPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = "Continue"
        $output = @(& docker compose --project-name $ProjectName --file $ComposeFile @ComposeArguments 2> $stderrFile)
        $exitCode = $LASTEXITCODE
        if ($exitCode -ne 0) {
            $stderr = Get-Content -LiteralPath $stderrFile -Raw
            throw "docker compose failed with exit code $exitCode`n$stderr"
        }
        return $output
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
        Remove-Item -LiteralPath $stderrFile -Force -ErrorAction SilentlyContinue
    }
}

function Invoke-Kafka {
    param([Parameter(Mandatory = $true)][string[]]$KafkaArguments)

    Invoke-Compose -ComposeArguments (@("exec", "-T", "kafka") + $KafkaArguments)
}

function Get-KafkaOutput {
    param([Parameter(Mandatory = $true)][string[]]$KafkaArguments)

    return Get-ComposeOutput -ComposeArguments (@("exec", "-T", "kafka") + $KafkaArguments)
}

function Wait-Healthy {
    for ($attempt = 1; $attempt -le 60; $attempt++) {
        $health = (& docker inspect --format "{{.State.Health.Status}}" $ContainerName 2>$null | Out-String).Trim()
        $state = (& docker inspect --format "{{.State.Status}}" $ContainerName 2>$null | Out-String).Trim()

        if ($health -eq "healthy") {
            Write-TestLog "Kafka container is healthy."
            return
        }
        if ($state -eq "exited" -or $state -eq "dead") {
            $logs = (& docker logs $ContainerName 2>&1 | Out-String).Trim()
            throw "Kafka container stopped before becoming healthy.`n$logs"
        }
        Start-Sleep -Seconds 2
    }

    $logs = (& docker logs $ContainerName 2>&1 | Out-String).Trim()
    throw "Kafka health check timed out.`n$logs"
}

function Ensure-Healthy {
    Invoke-Compose -ComposeArguments @("up", "-d")
    Wait-Healthy
}

function Seed-Fixture {
    Ensure-Healthy
    Invoke-Kafka -KafkaArguments @(
        "/opt/kafka/bin/kafka-topics.sh",
        "--bootstrap-server", "kafka:9092",
        "--create", "--if-not-exists",
        "--topic", $TopicName,
        "--partitions", "3",
        "--replication-factor", "1"
    )

    $producerArguments = @(
        "exec", "-T", "kafka",
        "/opt/kafka/bin/kafka-console-producer.sh",
        "--bootstrap-server", "kafka:9092",
        "--topic", $TopicName,
        "--property", "parse.key=true",
        "--property", "key.separator=|"
    )
    Get-Content -LiteralPath $FixtureFile | & docker compose --project-name $ProjectName --file $ComposeFile @producerArguments
    if ($LASTEXITCODE -ne 0) {
        throw "Kafka fixture producer failed with exit code $LASTEXITCODE"
    }
    Write-TestLog "Seeded $TopicName from $FixtureFile."
}

function Verify-Fixture {
    $consumerOutput = Get-KafkaOutput -KafkaArguments @(
        "/opt/kafka/bin/kafka-console-consumer.sh",
        "--bootstrap-server", "kafka:9092",
        "--topic", $TopicName,
        "--from-beginning",
        "--timeout-ms", "5000",
        "--property", "print.key=true",
        "--property", "key.separator=|"
    )
    $receivedLines = @($consumerOutput | ForEach-Object { $_.ToString().Trim() })

    foreach ($expectedLine in (Get-Content -LiteralPath $FixtureFile)) {
        if ($receivedLines -notcontains $expectedLine) {
            throw "Kafka fixture verification failed; missing record: $expectedLine`nReceived:`n$($receivedLines -join "`n")"
        }
    }
    Write-TestLog "Verified all fixture records in $TopicName."
}

function Run-RustIntegrationTest {
    $oldBootstrap = [Environment]::GetEnvironmentVariable("RAMAG_TEST_KAFKA_BOOTSTRAP", "Process")
    $oldTargetDirectory = [Environment]::GetEnvironmentVariable("CARGO_TARGET_DIR", "Process")
    $env:RAMAG_TEST_KAFKA_BOOTSTRAP = $BootstrapServers
    $env:CARGO_TARGET_DIR = Join-Path ([System.IO.Path]::GetTempPath()) "ramag-kafka-docker-target"

    $programFilesX86 = [Environment]::GetEnvironmentVariable("ProgramFiles(x86)")
    $vcVarsCandidates = @(
        (Join-Path $env:ProgramFiles "Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"),
        (Join-Path $programFilesX86 "Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat")
    )
    $vcVars = $vcVarsCandidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
    if ($null -eq $vcVars) {
        throw "Visual Studio 2022 C++ build tools with vcvars64.bat are required for cmake-build"
    }

    try {
        $cargoCommand = '"{0}" >nul && set "CMAKE_GENERATOR=NMake Makefiles" && set "RUSTFLAGS=-C target-feature=-crt-static" && cargo test --offline --locked -p ramag-infra-kafka --no-default-features --features cmake-build --test docker_kafka' -f $vcVars
        & cmd.exe /d /s /c $cargoCommand
        if ($LASTEXITCODE -ne 0) {
            throw "Rust Kafka integration test failed with exit code $LASTEXITCODE"
        }
    } finally {
        if ($null -eq $oldBootstrap) {
            Remove-Item Env:RAMAG_TEST_KAFKA_BOOTSTRAP -ErrorAction SilentlyContinue
        } else {
            $env:RAMAG_TEST_KAFKA_BOOTSTRAP = $oldBootstrap
        }
        if ($null -eq $oldTargetDirectory) {
            Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
        } else {
            $env:CARGO_TARGET_DIR = $oldTargetDirectory
        }
    }
}

function Run-IntegrationTest {
    Seed-Fixture
    Verify-Fixture
    Run-RustIntegrationTest
    Write-TestLog "Docker Kafka integration test passed."
}

if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
    throw "docker command is required"
}

switch ($Command) {
    "up" {
        Ensure-Healthy
    }
    "status" {
        Invoke-Compose -ComposeArguments @("ps")
    }
    "seed" {
        Seed-Fixture
    }
    "test" {
        Run-IntegrationTest
    }
    "down" {
        Invoke-Compose -ComposeArguments @("down", "--remove-orphans")
    }
    "clean" {
        Invoke-Compose -ComposeArguments @("down", "--volumes", "--remove-orphans")
        Write-TestLog "Removed the dedicated Kafka test container, network, and volume."
    }
}
