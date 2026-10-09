# Fetches the msedgedriver the E2E tests use, pinned to the installed WebView2 version
# (phase1-plan §5). Checks the zip's SHA-256 against the value recorded here and the
# exe's Authenticode signature (Microsoft doesn't publish checksums for the driver).
# Usage: powershell -NoProfile -File app\e2e\fetch-driver.ps1
# When WebView2 updates itself, the tests fail with the new version: add a row below
# after downloading it once and checking the signature.
$ErrorActionPreference = 'Stop'

$Pinned = @{
    # version          = SHA-256 of edgedriver_win64.zip (recorded 2026-10-05)
    '154.0.4258.53' = '16ACCCCA3DD71DA28348A5A6852B9FA681D1F177F758339EABACFBD3CAC8D72B'
    # recorded 2026-10-09 (WebView2 updated itself); signature Valid, O=Microsoft Corporation
    '154.0.4258.62' = 'C99F91B608E165831251DBD01245996BCF9FDE3932D56BD39BED297C1407B42E'
}

function Get-WebView2Version {
    foreach ($key in 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
                     'HKCU:\Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}') {
        $pv = (Get-ItemProperty -Path $key -Name pv -ErrorAction SilentlyContinue).pv
        if ($pv -and $pv -ne '0.0.0.0') { return $pv }
    }
    throw 'WebView2 runtime not found in the registry.'
}

$version = Get-WebView2Version
$hash = $Pinned[$version]
if (-not $hash) {
    throw "WebView2 is now $version and no msedgedriver hash is recorded for it: add one to $PSCommandPath."
}

$dest = Join-Path $PSScriptRoot ".drivers\$version"
$exe = Join-Path $dest 'msedgedriver.exe'
if (-not (Test-Path $exe)) {
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    $zip = Join-Path $dest 'edgedriver_win64.zip'
    $ProgressPreference = 'SilentlyContinue'
    Invoke-WebRequest -Uri "https://msedgedriver.microsoft.com/$version/edgedriver_win64.zip" -OutFile $zip -UseBasicParsing
    $got = (Get-FileHash -Path $zip -Algorithm SHA256).Hash
    if ($got -ne $hash) {
        Remove-Item -Recurse -Force $dest
        throw "msedgedriver $version zip has SHA-256 $got, expected $hash. Deleted."
    }
    Expand-Archive -Path $zip -DestinationPath $dest -Force
    Remove-Item $zip
}

$sig = Get-AuthenticodeSignature -FilePath $exe
# Status enum name and the certificate subject are not localized.
if ($sig.Status.ToString() -ne 'Valid' -or $sig.SignerCertificate.Subject -notmatch '(^|, )O=Microsoft Corporation(,|$)') {
    throw "msedgedriver.exe signature check failed: $($sig.Status) $($sig.SignerCertificate.Subject)"
}
Write-Output $exe
