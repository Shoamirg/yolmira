# Phase 1 manual exit check: pipe-owner rejection (H1), phase1-plan §4.
#
# Run as a SECOND Windows account while the first user's daemon is stopped. Creates
# \\.\pipe\amiri-<first user's SID> with an Everyone DACL, accepts every connection,
# and logs how many bytes each client sent. The first user's `amiri daemon status` and
# `amiri daemon start` must refuse this pipe: every connection logs 0 bytes (no token).
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File C:\Users\Public\impostor-pipe.ps1 -Sid S-1-5-21-...
#
# Windows PowerShell 5.1; needs no elevation. Stops after -Minutes or Ctrl+C.
param(
    [Parameter(Mandatory = $true)][string]$Sid,
    [string]$Suffix = '',
    [int]$Minutes = 15,
    [string]$Log = 'C:\Users\Public\amiri-impostor.log'
)
$ErrorActionPreference = 'Stop'
$name = "amiri-$Sid$Suffix"

function Write-Log([string]$line) {
    $stamped = "{0} {1}" -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $line
    Write-Output $stamped
    Add-Content -Path $Log -Value $stamped -Encoding UTF8
}

$security = New-Object System.IO.Pipes.PipeSecurity
$everyone = New-Object System.Security.Principal.SecurityIdentifier 'S-1-1-0'
$rule = New-Object System.IO.Pipes.PipeAccessRule($everyone, 'FullControl', 'Allow')
$security.AddAccessRule($rule)
$server = New-Object System.IO.Pipes.NamedPipeServerStream(
    $name, 'InOut', 1, 'Byte', 'Asynchronous', 4096, 4096, $security)

$me = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
Write-Log "impostor pipe \\.\pipe\$name created by $me (pid $PID); waiting $Minutes min"
$deadline = (Get-Date).AddMinutes($Minutes)
$connections = 0
$buffer = New-Object byte[] 65536
while ((Get-Date) -lt $deadline) {
    $wait = $server.BeginWaitForConnection($null, $null)
    while (-not $wait.AsyncWaitHandle.WaitOne(500)) {
        if ((Get-Date) -ge $deadline) { break }
    }
    if (-not $wait.IsCompleted) { break }
    $server.EndWaitForConnection($wait)
    $connections++
    # Count what the client sends within 5 s; a refusing client closes without writing.
    $received = 0
    $until = (Get-Date).AddSeconds(5)
    while ((Get-Date) -lt $until) {
        $read = $server.BeginRead($buffer, 0, $buffer.Length, $null, $null)
        $left = [int][Math]::Max(0, ($until - (Get-Date)).TotalMilliseconds)
        if (-not $read.AsyncWaitHandle.WaitOne($left)) { break }  # silent client
        try { $n = $server.EndRead($read) } catch { $n = 0 }        # client gone
        if ($n -eq 0) { break }
        $received += $n
    }
    Write-Log "connection $connections : $received bytes received"
    try { $server.Disconnect() } catch { }
}
$server.Dispose()
Write-Log "done: $connections connection(s)"
