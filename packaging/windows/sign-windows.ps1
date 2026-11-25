# SPDX-License-Identifier: MIT OR Apache-2.0
param(
    [Parameter(Mandatory = $true)]
    [string[]]$Files
)
$ErrorActionPreference = "Stop"

$pfxBase64 = $env:WINDOWS_SIGN_PFX_BASE64
$pfxPassword = $env:WINDOWS_SIGN_PFX_PASSWORD
$certThumb = $env:WINDOWS_SIGN_CERT_THUMBPRINT
$timestampUrl = if ([string]::IsNullOrWhiteSpace($env:WINDOWS_SIGN_TIMESTAMP_URL)) {
    "http://timestamp.digicert.com"
} else {
    $env:WINDOWS_SIGN_TIMESTAMP_URL
}
$requireSign = ($env:WINDOWS_SIGN_REQUIRE -eq "1")

$hasPfx = -not [string]::IsNullOrWhiteSpace($pfxBase64)
$hasThumb = -not [string]::IsNullOrWhiteSpace($certThumb)

if (-not $hasPfx -and -not $hasThumb) {
    if ($requireSign) {
        throw "WINDOWS_SIGN_REQUIRE=1 but neither WINDOWS_SIGN_PFX_BASE64 nor WINDOWS_SIGN_CERT_THUMBPRINT is set"
    }
    Write-Warning "Authenticode signing skipped (no signing credentials configured)."
    exit 0
}

$signtool = Get-Command "signtool.exe" -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source
if (-not $signtool) {
    $kitSigntool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending | Select-Object -First 1
    if ($kitSigntool) {
        $signtool = $kitSigntool.FullName
    }
}
if (-not $signtool) {
    throw "signtool.exe not found in PATH or Windows Kits 10 SDK"
}

$tempPfx = $null
try {
    if ($hasPfx) {
        $tempPfx = Join-Path $env:TEMP ("coree-sign-" + [Guid]::NewGuid().ToString("N") + ".pfx")
        [IO.File]::WriteAllBytes($tempPfx, [Convert]::FromBase64String($pfxBase64))
    }

    foreach ($file in $Files) {
        if (-not (Test-Path $file)) {
            throw "Cannot sign missing artifact: $file"
        }
        $signArgs = @("sign", "/fd", "SHA256", "/tr", $timestampUrl, "/td", "SHA256",
            "/d", "coree Vector Graphics Studio", "/du", "https://github.com/ahrrfy/coree")
        if ($hasPfx) {
            $signArgs += @("/f", $tempPfx)
            if (-not [string]::IsNullOrEmpty($pfxPassword)) {
                $signArgs += @("/p", $pfxPassword)
            }
        } else {
            $signArgs += @("/sha1", $certThumb, "/sm")
        }
        $signArgs += $file

        Write-Host "Signing $file ..."
        & $signtool @signArgs
        if ($LASTEXITCODE -ne 0) {
            throw "signtool sign failed for $file (exit code $LASTEXITCODE)"
        }

        & $signtool verify /pa $file
        if ($LASTEXITCODE -ne 0) {
            throw "signtool verify failed for $file (exit code $LASTEXITCODE)"
        }
    }
} finally {
    if ($tempPfx -and (Test-Path $tempPfx)) {
        Remove-Item -Force -Path $tempPfx -ErrorAction SilentlyContinue
    }
}
