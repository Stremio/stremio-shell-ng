@echo off
set mypath=%~dp0

:: Compile the main executable
if not exist "%mypath%..\target\release\stremio-shell-ng.exe" (
    cargo build --release
) else (
    echo Main executable is already built
)

:: Compile the installer
:: "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" "%mypath%Stremio.iss"
"C:\Program Files (x86)\Inno Setup 6\ISCC.exe" "/Sstremiosign=$qC:\Program Files (x86)\Windows Kits\10\bin\10.0.17763.0\x86\signtool.exe$q sign /f $q${{ github.workspace }}\certificates\smartcode-20211118-20241118.pfx$q /p ${{ secrets.WIN_CERT_PASSWORD }} /v $f" "%mypath%Stremio.iss"