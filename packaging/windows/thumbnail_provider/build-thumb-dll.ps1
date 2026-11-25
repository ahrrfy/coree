# SPDX-License-Identifier: MIT OR Apache-2.0
param(
    [string]$OutDir = "target\release"
)
$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = Resolve-Path (Join-Path $scriptDir "..\..\..")
$outAbs = if ([System.IO.Path]::IsPathRooted($OutDir)) { $OutDir } else { Join-Path $repoRoot $OutDir }
New-Item -ItemType Directory -Force -Path $outAbs | Out-Null
$outDll = Join-Path $outAbs "coree_thumb.dll"

$cpp = Join-Path $scriptDir "coree_thumb.cpp"
$rc = Join-Path $scriptDir "coree_thumb.rc"
$def = Join-Path $scriptDir "coree_thumb.def"
$res = Join-Path $outAbs "coree_thumb.res"

$cl = Get-Command "cl.exe" -ErrorAction SilentlyContinue
if ($cl) {
    $rcExe = Get-Command "rc.exe" -ErrorAction SilentlyContinue
    if (-not $rcExe) {
        $sdkBin = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\rc.exe" -ErrorAction SilentlyContinue |
            Sort-Object FullName -Descending | Select-Object -First 1
        if ($sdkBin) { $rcExe = $sdkBin.FullName }
    } else {
        $rcExe = $rcExe.Source
    }
    if (-not $rcExe) {
        throw "rc.exe not found in PATH or Windows Kits 10 SDK"
    }

    & $rcExe /nologo /fo $res $rc
    if ($LASTEXITCODE -ne 0) { throw "rc.exe failed with exit code $LASTEXITCODE" }

    & $cl.Source /nologo /O2 /LD /EHsc /std:c++17 /DUNICODE /D_UNICODE $cpp $res `
        /link /DEF:$def ole32.lib oleaut32.lib shlwapi.lib gdi32.lib user32.lib shell32.lib windowscodecs.lib advapi32.lib `
        /OUT:$outDll
    if ($LASTEXITCODE -ne 0) { throw "cl.exe failed with exit code $LASTEXITCODE" }
} else {
    $mingw = Get-Command "x86_64-w64-mingw32-g++" -ErrorAction SilentlyContinue
    if (-not $mingw) {
        throw "Neither MSVC cl.exe nor x86_64-w64-mingw32-g++ found on PATH"
    }
    $windres = Get-Command "x86_64-w64-mingw32-windres" -ErrorAction SilentlyContinue
    $objArgs = @()
    if ($windres) {
        $resObj = Join-Path $outAbs "coree_thumb_res.o"
        & $windres.Source $rc -O coff -o $resObj
        if ($LASTEXITCODE -ne 0) { throw "windres failed with exit code $LASTEXITCODE" }
        $objArgs += $resObj
    }
    & $mingw.Source -O2 -shared -std=c++17 -DUNICODE -D_UNICODE $cpp $def @objArgs `
        -o $outDll -lole32 -loleaut32 -lshlwapi -lgdi32 -luser32 -lshell32 -lwindowscodecs -ladvapi32 `
        -Wl,--kill-at
    if ($LASTEXITCODE -ne 0) { throw "MinGW build failed with exit code $LASTEXITCODE" }
}

if (-not (Test-Path $outDll)) {
    throw "Expected output DLL missing: $outDll"
}
Write-Host "Built Windows thumbnail provider: $outDll"
