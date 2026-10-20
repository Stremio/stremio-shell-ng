@echo off
set mypath=%~dp0

:: Compile the main executable
if not exist "%mypath%..\target\release\stremio-shell-ng.exe" (
    cargo build --release
) else (
    echo Main executable is already built
)

:: Compile the installer
set "ISCC=C:\Program Files (x86)\Inno Setup 6\ISCC.exe"
if not exist "%ISCC%" set "ISCC=%LOCALAPPDATA%\Programs\Inno Setup 6\ISCC.exe"
if not exist "%ISCC%" (
    echo ISCC.exe introuvable : installer Inno Setup 6
    exit /b 1
)
"%ISCC%" "%mypath%Laststream.iss"
