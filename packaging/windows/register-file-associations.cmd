@echo off
chcp 65001 >nul 2>&1
rem SPDX-License-Identifier: MIT OR Apache-2.0
rem Zero-admin HKCU file association installer for portable Windows coree builds.
rem Double-click to associate .coree, .cdr, .cdt, .cmx, .cpt, .svg, .dxf, .plt with coree.exe.
rem Pass /u or --unregister to remove HKCU registrations cleanly.

setlocal EnableExtensions

set "BASE_DIR=%~dp0"
set "EXE_PATH=%BASE_DIR%coree.exe"
set "ICO_PATH=%BASE_DIR%coree.ico"
set "THUMB_DLL=%BASE_DIR%coree_thumb.dll"
set "THUMB_CLSID={B4E8A7C2-5F19-4D3A-9C6E-1A2B3C4D5E6F}"
set "THUMB_IID={e357fccd-a995-4576-b01f-234630154e96}"

if /I "%~1"=="/u" goto :unregister
if /I "%~1"=="--unregister" goto :unregister

if not exist "%EXE_PATH%" (
    echo [coree] Error: coree.exe not found next to this script:
    echo         "%EXE_PATH%"
    echo         Keep this script in the same folder as coree.exe and run it again.
    pause
    exit /b 1
)

if exist "%ICO_PATH%" (
    set "ICON_SPEC=%ICO_PATH%,0"
) else (
    set "ICON_SPEC=%EXE_PATH%,0"
)

echo [coree] Registering per-user (HKCU) file associations for:
echo         "%EXE_PATH%"
echo.

reg add "HKCU\Software\Classes\coree.Document" /ve /t REG_SZ /d "coree Vector Graphics Document" /f >nul
reg add "HKCU\Software\Classes\coree.Document\DefaultIcon" /ve /t REG_SZ /d "%ICON_SPEC%" /f >nul
reg add "HKCU\Software\Classes\coree.Document\shell\open" /ve /t REG_SZ /d "Open with coree" /f >nul
reg add "HKCU\Software\Classes\coree.Document\shell\open\command" /ve /t REG_SZ /d "\"%EXE_PATH%\" \"%%1\"" /f >nul

reg add "HKCU\Software\Classes\Applications\coree.exe" /v "FriendlyAppName" /t REG_SZ /d "coree Vector Graphics Studio" /f >nul
reg add "HKCU\Software\Classes\Applications\coree.exe\DefaultIcon" /ve /t REG_SZ /d "%ICON_SPEC%" /f >nul
reg add "HKCU\Software\Classes\Applications\coree.exe\shell\open\command" /ve /t REG_SZ /d "\"%EXE_PATH%\" \"%%1\"" /f >nul

rem Primary design formats (default ProgID + OpenWithProgids)
for %%E in (.coree .cdr .cdt .cmx .cpt) do (
    reg add "HKCU\Software\Classes\%%E" /ve /t REG_SZ /d "coree.Document" /f >nul
    reg add "HKCU\Software\Classes\%%E\OpenWithProgids" /v "coree.Document" /t REG_SZ /d "" /f >nul
    reg add "HKCU\Software\Classes\Applications\coree.exe\SupportedTypes" /v "%%E" /t REG_SZ /d "" /f >nul
)

rem Interchange & laser/plotter formats (OpenWithProgids without hijacking existing defaults)
for %%E in (.svg .dxf .plt .pdf .eps .psd) do (
    reg add "HKCU\Software\Classes\%%E\OpenWithProgids" /v "coree.Document" /t REG_SZ /d "" /f >nul
    reg add "HKCU\Software\Classes\Applications\coree.exe\SupportedTypes" /v "%%E" /t REG_SZ /d "" /f >nul
)

rem Optional Explorer IThumbnailProvider registration when coree_thumb.dll sits beside coree.exe
if exist "%THUMB_DLL%" (
    reg add "HKCU\Software\Classes\CLSID\%THUMB_CLSID%" /ve /t REG_SZ /d "coree Thumbnail Provider" /f >nul
    reg add "HKCU\Software\Classes\CLSID\%THUMB_CLSID%\InprocServer32" /ve /t REG_SZ /d "%THUMB_DLL%" /f >nul
    reg add "HKCU\Software\Classes\CLSID\%THUMB_CLSID%\InprocServer32" /v "ThreadingModel" /t REG_SZ /d "Apartment" /f >nul
    reg add "HKCU\Software\Classes\coree.Document\ShellEx\%THUMB_IID%" /ve /t REG_SZ /d "%THUMB_CLSID%" /f >nul
    for %%E in (.coree .cdr .cdt .cmx .cpt) do (
        reg add "HKCU\Software\Classes\%%E\ShellEx\%THUMB_IID%" /ve /t REG_SZ /d "%THUMB_CLSID%" /f >nul
    )
)

rem Refresh Windows Explorer icon cache
if exist "%SystemRoot%\System32\ie4uinit.exe" (
    "%SystemRoot%\System32\ie4uinit.exe" -show >nul 2>&1
)

echo [coree] Done! File associations registered for the current user (zero admin required):
echo         Default handler: .coree, .cdr, .cdt, .cmx, .cpt
echo         Open With menu:  .svg, .dxf, .plt, .pdf, .eps, .psd
echo.
echo Tip: To undo later, run: "%~nx0" --unregister
if "%~1"=="" pause
exit /b 0

:unregister
echo [coree] Removing per-user (HKCU) file associations...

rem Clear (Default) on primary extensions only when still pointing to coree.Document (locale-safe)
for %%E in (.coree .cdr .cdt .cmx .cpt) do (
    reg query "HKCU\Software\Classes\%%E" /ve 2>nul | findstr /I /E /C:"    REG_SZ    coree.Document" >nul 2>&1
    if not errorlevel 1 (
        reg delete "HKCU\Software\Classes\%%E" /ve /f >nul 2>&1
    )
)

for %%E in (.coree .cdr .cdt .cmx .cpt .svg .dxf .plt .pdf .eps .psd) do (
    reg delete "HKCU\Software\Classes\%%E\OpenWithProgids" /v "coree.Document" /f >nul 2>&1
)

for %%E in (.coree .cdr .cdt .cmx .cpt) do (
    reg delete "HKCU\Software\Classes\%%E\ShellEx\%THUMB_IID%" /f >nul 2>&1
)
reg delete "HKCU\Software\Classes\coree.Document\ShellEx\%THUMB_IID%" /f >nul 2>&1
reg delete "HKCU\Software\Classes\CLSID\%THUMB_CLSID%" /f >nul 2>&1

reg delete "HKCU\Software\Classes\coree.Document" /f >nul 2>&1
reg delete "HKCU\Software\Classes\Applications\coree.exe" /f >nul 2>&1

if exist "%SystemRoot%\System32\ie4uinit.exe" (
    "%SystemRoot%\System32\ie4uinit.exe" -show >nul 2>&1
)
echo [coree] Unregistered per-user file associations cleanly.
if "%~2"=="" pause
exit /b 0
