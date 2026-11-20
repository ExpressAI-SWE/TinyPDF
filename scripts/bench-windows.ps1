param(
    [Parameter(Mandatory)]
    [string] $Executable,

    [string] $Pdf,

    [ValidateRange(1, 20)]
    [int] $Runs = 3,

    [ValidateRange(1, 60)]
    [int] $IdleSeconds = 5,

    [ValidateRange(1, 60)]
    [int] $WarmupSeconds = 5,

    [switch] $Browser
)

$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'Windows process measurements require Windows.' }
$Executable = (Resolve-Path -LiteralPath $Executable).Path
if ($Pdf) { $Pdf = (Resolve-Path -LiteralPath $Pdf).Path }
$root = Split-Path -Parent $PSScriptRoot
$benchmarkRoot = (New-Item -ItemType Directory -Path (Join-Path $root 'target/benchmark') -Force).FullName
try {
    $null = Get-CimInstance -Query 'SELECT ProcessId FROM Win32_Process' -ErrorAction Stop
} catch {
    throw "Windows denied the process snapshot required for benchmarking. $($_.Exception.Message)"
}

function Get-ReaderProcessTree([int] $RootId, [datetime] $Launched) {
    $processes = Get-CimInstance -Query 'SELECT ProcessId,ParentProcessId FROM Win32_Process'
    $descendants = [System.Collections.Generic.HashSet[int]]::new()
    $null = $descendants.Add($RootId)
    do {
        $count = $descendants.Count
        foreach ($process in $processes) {
            if ($descendants.Contains([int]$process.ParentProcessId)) {
                $null = $descendants.Add([int]$process.ProcessId)
            }
        }
    } while ($count -ne $descendants.Count)
    Get-Process -Id ([int[]]@($descendants)) -ErrorAction SilentlyContinue |
        Where-Object { $_.StartTime -ge $Launched.AddSeconds(-1) }
}

function Measure-ReaderGpuMemory([int[]] $ProcessIds) {
    try {
        $samples = (Get-Counter '\GPU Process Memory(*)\Dedicated Usage', '\GPU Process Memory(*)\Shared Usage' -ErrorAction Stop).CounterSamples
    } catch {
        return $null
    }
    $dedicated = 0
    $shared = 0
    $matched = $false
    foreach ($sample in $samples) {
        if ($sample.InstanceName -match '^pid_(\d+)_' -and [int]$Matches[1] -in $ProcessIds) {
            $matched = $true
            if ($sample.Path -like '*\dedicated usage') { $dedicated += $sample.CookedValue }
            if ($sample.Path -like '*\shared usage') { $shared += $sample.CookedValue }
        }
    }
    if ($matched) {
        @{ DedicatedBytes = [long]$dedicated; SharedBytes = [long]$shared }
    }
}

$measurements = for ($run = 1; $run -le $Runs; $run++) {
    $profile = (New-Item -ItemType Directory -Path (Join-Path $benchmarkRoot ([guid]::NewGuid()))).FullName
    $start = [System.Diagnostics.ProcessStartInfo]::new($Executable)
    $start.UseShellExecute = $false
    if ($Browser) {
        foreach ($arg in @("--user-data-dir=$profile", '--no-first-run', '--no-default-browser-check', '--disable-background-networking', '--disable-extensions', '--new-window')) {
            $start.ArgumentList.Add($arg)
        }
        if ($Pdf) { $start.ArgumentList.Add(([uri]$Pdf).AbsoluteUri) }
        else { $start.ArgumentList.Add('about:blank') }
    } else {
        $start.Environment['TINYPDF_DATA_DIR'] = $profile
        if ($Pdf) {
            $start.ArgumentList.Add('--timing')
            $start.ArgumentList.Add($Pdf)
            $start.RedirectStandardError = $true
        }
    }

    $process = $null
    $stderr = $null
    $tree = @()
    try {
        $launched = Get-Date
        $clock = [System.Diagnostics.Stopwatch]::StartNew()
        $process = [System.Diagnostics.Process]::Start($start)
        if ($start.RedirectStandardError) { $stderr = $process.StandardError.ReadToEndAsync() }
        while ($true) {
            $process.Refresh()
            if ($process.HasExited) { throw "The reader exited before producing a window (status $($process.ExitCode))." }
            if ($process.MainWindowHandle -ne 0) { break }
            if ($clock.Elapsed.TotalSeconds -ge 30) { throw 'Timed out waiting for a reader window.' }
            Start-Sleep -Milliseconds 20
        }
        $startup = $clock.Elapsed.TotalMilliseconds
        Start-Sleep -Seconds $WarmupSeconds

        $tree = @(Get-ReaderProcessTree $process.Id $launched)
        if (-not $tree) { throw 'No reader processes were available to measure.' }
        $reported = @{}
        $startingCpu = 0.0
        foreach ($item in $tree) { $reported[$item.Id] = $item.CPU; $startingCpu += $item.CPU }
        $peakWorking = 0L
        $idleClock = [System.Diagnostics.Stopwatch]::StartNew()
        do {
            $tree = @(Get-ReaderProcessTree $process.Id $launched)
            if (-not $tree) { throw 'The reader exited during measurements.' }
            $working = [long]($tree | Measure-Object WorkingSet64 -Sum).Sum
            $peakWorking = [Math]::Max($peakWorking, $working)
            foreach ($item in $tree) {
                $reported[$item.Id] = [Math]::Max([double]$reported[$item.Id], [double]$item.CPU)
            }
            $remaining = $IdleSeconds * 1000 - $idleClock.ElapsedMilliseconds
            if ($remaining -le 0) { break }
            Start-Sleep -Milliseconds ([math]::Min(450, $remaining))
        } while ($true)
        $idleClock.Stop()
        $privateBytes = [long]($tree | Measure-Object PrivateMemorySize64 -Sum).Sum
        $idleCpu = ([double]($reported.Values | Measure-Object -Sum).Sum - $startingCpu)
        $idleCpuPercent = 100.0 * $idleCpu / $idleClock.Elapsed.TotalSeconds
        $gpu = Measure-ReaderGpuMemory @($tree.Id)

        $measurement = [PSCustomObject]@{
            Run = $run
            Executable = $Executable
            ExecutableBytes = (Get-Item -LiteralPath $Executable).Length
            DocumentBytes = if ($Pdf) { (Get-Item -LiteralPath $Pdf).Length }
            ProcessCount = $tree.Count
            WindowStartupMs = [Math]::Round($startup, 1)
            FirstTileMs = $null
            WarmupSeconds = $WarmupSeconds
            SampleSeconds = [Math]::Round($idleClock.Elapsed.TotalSeconds, 3)
            WorkingSetBytes = $working
            SampledPeakWorkingSetBytes = $peakWorking
            PrivateBytes = $privateBytes
            IdleCpuPercentOfOneCore = [Math]::Round($idleCpuPercent, 3)
            GpuDedicatedBytes = $gpu.DedicatedBytes
            GpuSharedBytes = $gpu.SharedBytes
        }
    } finally {
        if ($process -and -not $process.HasExited) {
            $null = $process.CloseMainWindow()
            if (-not $process.WaitForExit(3000)) {
                $process.Kill($true)
                $null = $process.WaitForExit(3000)
            }
        }
        foreach ($item in $tree) {
            if ($item.Id -ne $process.Id -and -not $item.HasExited -and $item.StartTime -ge $launched) {
                try { $item.Kill() } catch [System.InvalidOperationException] {}
            }
        }
        $profilePath = (Resolve-Path -LiteralPath $profile).Path
        if ((Split-Path -Parent $profilePath) -eq $benchmarkRoot -and (Split-Path -Leaf $profilePath) -as [guid]) {
            Remove-Item -LiteralPath $profilePath -Recurse -Force
        }
    }
    if ($stderr) {
        if (-not $stderr.Wait(2000) -or $stderr.Result -notmatch '(?m)^first_tile_ms=([\d.]+)\s*$') {
            throw 'TinyPDF did not publish a first-tile timing for the requested document.'
        }
        $measurement.FirstTileMs = [double]::Parse($Matches[1], [Globalization.CultureInfo]::InvariantCulture)
    }
    $measurement
}

$measurements | ConvertTo-Json -Depth 3 -AsArray
