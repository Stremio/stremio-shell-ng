@echo off
set mypath=%~dp0

:: Compile the main executable
if not exist "%mypath%..\target\release\stremio-shell-ng.exe" (
    cargo build --release
) else (
    echo Main executable is already built
)

:: Compile the installer
"C:\Program Files (x86)\Inno Setup 6\ISCC.exe" "%mypath%Stremio.iss"
