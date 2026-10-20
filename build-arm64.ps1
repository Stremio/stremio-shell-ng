cargo build --release --target aarch64-pc-windows-msvc
Write-Host "Binaire ARM64 : target\aarch64-pc-windows-msvc\release\stremio-shell-ng.exe"
Write-Host "Aucun installeur ARM64 : setup\Laststream.iss ne cible que x64compatible (pas de variante arm64)."
