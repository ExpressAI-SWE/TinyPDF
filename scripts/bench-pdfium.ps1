param(
    [Parameter(Mandatory)]
    [string] $Executable,

    [Parameter(Mandatory)]
    [string] $Pdf,

    [ValidateRange(1, 20)]
    [int] $Runs = 3
)

$ErrorActionPreference = 'Stop'
$Executable = (Resolve-Path -LiteralPath $Executable).Path
$Pdf = (Resolve-Path -LiteralPath $Pdf).Path
$measurements = for ($run = 1; $run -le $Runs; $run++) {
    $start = [System.Diagnostics.ProcessStartInfo]::new($Executable)
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.ArgumentList.Add('--benchmark')
    $start.ArgumentList.Add($Pdf)
    $process = [System.Diagnostics.Process]::Start($start)
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(120000)) {
            $process.Kill($true)
            throw 'TinyPDF benchmark exceeded two minutes.'
        }
        if ($process.ExitCode -ne 0) {
            throw "TinyPDF benchmark failed (status $($process.ExitCode)). $($stderr.GetAwaiter().GetResult())"
        }
        $result = $stdout.GetAwaiter().GetResult() | ConvertFrom-Json
        if (-not $result -or $result.pages -lt 1 -or -not $result.render_ms) {
            throw 'TinyPDF did not produce a valid engine benchmark result.'
        }
        [PSCustomObject]@{
            Run = $run
            DocumentBytes = (Get-Item -LiteralPath $Pdf).Length
            Pages = $result.pages
            EngineMs = $result.engine_ms
            OpenMs = $result.open_ms
            RenderMs = $result.render_ms
            SearchMs = $result.search_ms
            SearchMatches = $result.search_matches
        }
    } finally {
        $process.Dispose()
    }
}
$measurements | ConvertTo-Json -Depth 3 -AsArray
