# Runs every workspace test binary directly, outside cargo's job object.
# cargo on Windows puts itself and its children in a kill-on-close job that forbids
# breakaway, so a detached daemon start (what the lifecycle tests check) is
# impossible under `cargo test`. Usage: powershell -File scripts\test.ps1 [-Release] [-Filter name]
param(
    [switch]$Release,
    [string]$Filter = ""
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    $cargoArgs = @('test', '--workspace', '--locked', '--no-run', '--message-format=json')
    if ($Release) { $cargoArgs += '--release' }
    $lines = & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { Write-Error "cargo build of tests failed ($LASTEXITCODE)"; exit 1 }
    $exes = $lines |
        Where-Object { $_.StartsWith('{') } |
        ForEach-Object { $_ | ConvertFrom-Json } |
        Where-Object { $_.reason -eq 'compiler-artifact' -and $_.profile.test -and $_.executable } |
        ForEach-Object { $_.executable } |
        Sort-Object -Unique
    $failed = @()
    $notRun = @()
    $durations = @()
    foreach ($exe in $exes) {
        Write-Host "== $exe"
        $testArgs = @()
        if ($Filter) { $testArgs += $Filter }
        # cmd merges stderr into stdout as plain text (PowerShell 5.1 would wrap it in error records).
        $watch = [Diagnostics.Stopwatch]::StartNew()
        $out = & cmd /d /c "`"$exe`" $($testArgs -join ' ') 2>&1"
        $code = $LASTEXITCODE
        # Binary name without cargo's hash suffix, e.g. pty-3f2a9c... -> pty.
        $name = [IO.Path]::GetFileNameWithoutExtension($exe) -replace '-[0-9a-f]{16}$', ''
        $durations += '{0,-14} {1,7:N1} s  exit {2}' -f $name, $watch.Elapsed.TotalSeconds, $code
        $out | ForEach-Object { Write-Host $_ }
        $notRun += $out | Where-Object { $_ -match '^NOT RUN' }
        if ($code -ne 0) {
            # Keep the whole output: a rare failure is only worth something with its evidence.
            $logDir = Join-Path $root 'target\test-logs'
            New-Item -ItemType Directory -Force -Path $logDir | Out-Null
            $log = Join-Path $logDir ('{0}-{1}.log' -f $name, (Get-Date -Format 'yyyyMMdd-HHmmss'))
            $out | Out-File -FilePath $log -Encoding utf8
            $failed += "$exe (full output: $log)"
        }
    }
    Write-Host "Durations per test binary:"
    $durations | ForEach-Object { Write-Host "  $_" }
    if ($notRun.Count -gt 0) {
        Write-Host "Not run in this environment ($($notRun.Count)):"
        $notRun | ForEach-Object { Write-Host "  $_" }
    }
    if ($failed.Count -gt 0) {
        Write-Host "FAILED test binaries:"
        $failed | ForEach-Object { Write-Host "  $_" }
        exit 1
    }
    Write-Host "All $($exes.Count) test binaries passed."
}
finally {
    Pop-Location
}
