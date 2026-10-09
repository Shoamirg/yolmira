# Test helper (tests/connections.rs): from ONE process, open -Count connections to the
# daemon's pipe and authenticate each with hello. Prints "accepted <n>" once all attempts
# are done, then holds the accepted connections until stdin closes.
# Windows PowerShell 5.1; no elevation.
param(
    [Parameter(Mandatory = $true)][string]$Pipe,      # name without \\.\pipe\
    [Parameter(Mandatory = $true)][string]$TokenFile,
    [Parameter(Mandatory = $true)][int]$ProtocolVersion,
    [int]$Count = 128
)
$ErrorActionPreference = 'Stop'
$token = (Get-Content -LiteralPath $TokenFile -Raw).Trim()
$held = New-Object System.Collections.ArrayList
for ($i = 1; $i -le $Count; $i++) {
    $c = New-Object System.IO.Pipes.NamedPipeClientStream('.', $Pipe, 'InOut', 'Asynchronous')
    try {
        $c.Connect(5000)
        $hello = @{ jsonrpc = '2.0'; id = 1; method = 'hello';
                    params = @{ token = $token; protocol_version = $ProtocolVersion; client = "holder-$i" } } |
                 ConvertTo-Json -Compress -Depth 4
        $bytes = [System.Text.Encoding]::UTF8.GetBytes($hello + "`n")
        $c.Write($bytes, 0, $bytes.Length)
        $c.Flush()
        # Read the answer line; a refused connection is closed at once (0 bytes).
        $line = New-Object System.Collections.Generic.List[byte]
        $buf = New-Object byte[] 4096
        $until = (Get-Date).AddSeconds(5)
        $ok = $false
        while ((Get-Date) -lt $until) {
            $read = $c.BeginRead($buf, 0, $buf.Length, $null, $null)
            $left = [int][Math]::Max(1, ($until - (Get-Date)).TotalMilliseconds)
            if (-not $read.AsyncWaitHandle.WaitOne($left)) { break }
            $n = $c.EndRead($read)
            if ($n -eq 0) { break }
            $line.AddRange([byte[]]($buf[0..($n - 1)]))
            if ($line.Contains([byte]10)) {
                $text = [System.Text.Encoding]::UTF8.GetString($line.ToArray())
                $ok = $text -match '"result"'
                break
            }
        }
        if ($ok) { [void]$held.Add($c) } else { $c.Dispose() }
    } catch {
        $c.Dispose()
    }
}
[Console]::Out.WriteLine("accepted $($held.Count)")
[Console]::Out.Flush()
# Hold until the test closes stdin (the test also kills this process when it ends).
[void][Console]::In.ReadLine()
foreach ($c in $held) { $c.Dispose() }
