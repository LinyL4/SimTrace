param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('A', 'B', 'C', 'D', 'LONG')]
    [string]$Scenario,

    [ValidateRange(0, 600)]
    [int]$WarmupSeconds = 10,

    [ValidateRange(10, 1800)]
    [int]$DurationSeconds = 60,

    [ValidateRange(200, 5000)]
    [int]$SampleIntervalMilliseconds = 1000,

    [string]$OutputDirectory = 'benchmark-results'
)

$ErrorActionPreference = 'Stop'

$scenarioSettings = @{
    A = @{ WindowSeconds = 10; PhasePlot = $false }
    B = @{ WindowSeconds = 30; PhasePlot = $false }
    C = @{ WindowSeconds = 10; PhasePlot = $true }
    D = @{ WindowSeconds = 30; PhasePlot = $true }
    LONG = @{ WindowSeconds = $null; PhasePlot = $null }
}

$process = Get-Process -Name 'simtrace' -ErrorAction Stop | Select-Object -First 1
$logicalProcessors = [Environment]::ProcessorCount
$settings = $scenarioSettings[$Scenario]
$timestamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$root = [IO.Path]::GetFullPath((Join-Path (Get-Location) $OutputDirectory))
New-Item -ItemType Directory -Force -Path $root | Out-Null
$csvPath = Join-Path $root ("phase1-{0}-{1}.csv" -f $Scenario.ToLowerInvariant(), $timestamp)
$summaryPath = Join-Path $root ("phase1-{0}-{1}-summary.json" -f $Scenario.ToLowerInvariant(), $timestamp)

Write-Host ("Scenario {0}: window={1}, phase_plot={2}" -f $Scenario, $settings.WindowSeconds, $settings.PhasePlot)
Write-Host ("Process {0}, PID {1}; warm-up {2}s, measurement {3}s" -f $process.ProcessName, $process.Id, $WarmupSeconds, $DurationSeconds)

if ($WarmupSeconds -gt 0) {
    Start-Sleep -Seconds $WarmupSeconds
}

$rows = [Collections.Generic.List[object]]::new()
$process.Refresh()
$previousWall = [DateTime]::UtcNow
$previousCpuSeconds = $process.TotalProcessorTime.TotalSeconds
$measurementStarted = $previousWall
$startWorkingSet = $process.WorkingSet64
$startPrivate = $process.PrivateMemorySize64

while (([DateTime]::UtcNow - $measurementStarted).TotalSeconds -lt $DurationSeconds) {
    Start-Sleep -Milliseconds $SampleIntervalMilliseconds
    $process.Refresh()
    if ($process.HasExited) {
        throw 'SimTrace exited during measurement.'
    }

    $now = [DateTime]::UtcNow
    $cpuSeconds = $process.TotalProcessorTime.TotalSeconds
    $wallSeconds = ($now - $previousWall).TotalSeconds
    $singleCorePercent = (($cpuSeconds - $previousCpuSeconds) / $wallSeconds) * 100.0
    $machinePercent = $singleCorePercent / $logicalProcessors

    $rows.Add([pscustomobject]@{
        timestamp_utc = $now.ToString('o')
        elapsed_seconds = ($now - $measurementStarted).TotalSeconds
        cpu_single_core_percent = $singleCorePercent
        cpu_machine_percent = $machinePercent
        working_set_mib = $process.WorkingSet64 / 1MB
        private_memory_mib = $process.PrivateMemorySize64 / 1MB
        thread_count = $process.Threads.Count
        handle_count = $process.HandleCount
    })

    $previousWall = $now
    $previousCpuSeconds = $cpuSeconds
}

$rows | Export-Csv -LiteralPath $csvPath -NoTypeInformation -Encoding UTF8
$endWorkingSet = $process.WorkingSet64
$endPrivate = $process.PrivateMemorySize64
$cpuSingle = @($rows | ForEach-Object { $_.cpu_single_core_percent })
$cpuMachine = @($rows | ForEach-Object { $_.cpu_machine_percent })
$workingSets = @($rows | ForEach-Object { $_.working_set_mib })
$privateValues = @($rows | ForEach-Object { $_.private_memory_mib })

$summary = [ordered]@{
    scenario = $Scenario
    expected_window_seconds = $settings.WindowSeconds
    expected_phase_plot_open = $settings.PhasePlot
    process_id = $process.Id
    logical_processors = $logicalProcessors
    warmup_seconds = $WarmupSeconds
    measured_seconds = ([DateTime]::UtcNow - $measurementStarted).TotalSeconds
    samples = $rows.Count
    cpu_single_core_percent_mean = ($cpuSingle | Measure-Object -Average).Average
    cpu_single_core_percent_max = ($cpuSingle | Measure-Object -Maximum).Maximum
    cpu_machine_percent_mean = ($cpuMachine | Measure-Object -Average).Average
    cpu_machine_percent_max = ($cpuMachine | Measure-Object -Maximum).Maximum
    working_set_mib_start = $startWorkingSet / 1MB
    working_set_mib_end = $endWorkingSet / 1MB
    working_set_mib_delta = ($endWorkingSet - $startWorkingSet) / 1MB
    working_set_mib_max = ($workingSets | Measure-Object -Maximum).Maximum
    private_memory_mib_start = $startPrivate / 1MB
    private_memory_mib_end = $endPrivate / 1MB
    private_memory_mib_delta = ($endPrivate - $startPrivate) / 1MB
    private_memory_mib_max = ($privateValues | Measure-Object -Maximum).Maximum
    csv_path = $csvPath
}

$summary | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $summaryPath -Encoding UTF8
$summary | ConvertTo-Json -Depth 4
Write-Host ("CSV: {0}" -f $csvPath)
Write-Host ("Summary: {0}" -f $summaryPath)
