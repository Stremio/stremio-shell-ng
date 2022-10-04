@echo off
set mypath=%~dp0

:: Download ffmpeg and node
set missing=
if not exist "%mypath%..\bin" set missing=1
if not exist "%mypath%..\bin\node.exe" set missing=1
if not exist "%mypath%..\bin\ResourceHacker.exe" set  missing=1
if not exist "%mypath%..\bin\ffmpeg.exe" set missing=1
if not exist "%mypath%..\bin\ffprobe.exe" set missing=1
if defined missing (
    powershell -nologo -executionpolicy bypass -File "%mypath%get_exe_from_zip.ps1"
) else (
    echo Binaries for ffmpeg, ffprobe, node and ResHack are already present
)

:: Convert node to stremio-runtime
if not exist "%mypath%..\bin\stremio-runtime.exe" (
    call "%mypath%generate_stremio-runtime.bat" %mypath%..\bin
) else (
    echo The executable stremio-runtime.exe is already generated
)

:: Compile the main executable
if not exist "%mypath%..\target\release\stremio-shell-ng.exe" (
    cargo build --release
) else (
    echo Main executable is already built
)

:: Compile the installer
"C:\Program Files (x86)\Inno Setup 6\ISCC.exe" "%mypath%Stremio.iss"