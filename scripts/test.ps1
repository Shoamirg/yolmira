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
    foreach ($exe in $exes) {
        Write-Host "== $exe"
        if ($Filter) { & $exe $Filter } else { & $exe }
        if ($LASTEXITCODE -ne 0) { $failed += $exe }
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
