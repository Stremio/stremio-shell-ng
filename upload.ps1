# Publie l'installeur Windows de Laststream dans le bucket R2 laststream-app-files (cle windows/<nom>).
# Le Worker laststream-edge le sert ensuite sur /app/windows/<nom> (voir appFile() dans laststream-edge/src/index.js).
# Prerequis : `wrangler login` deja fait. Aucun secret n'est lu ni ecrit par ce script.
#   .\upload.ps1                       # prend le Laststream-Setup-X.Y.Z.exe le plus recent de ..\dist\installer
#   .\upload.ps1 -Installer <fichier>  # installeur precis
#   .\upload.ps1 -DryRun               # affiche la commande sans rien envoyer
param(
    [string]$Installer,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
$bucket = 'laststream-app-files'
$publicBase = 'https://laststream.gtux-prog.workers.dev/app/windows'

if (-not $Installer) {
    $Installer = Get-ChildItem (Join-Path $PSScriptRoot '..\dist\installer') -Filter 'Laststream-Setup-*.exe' -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1 -ExpandProperty FullName
}
if (-not $Installer -or -not (Test-Path $Installer)) { throw 'Installeur introuvable : lancer build.ps1 ou passer -Installer <fichier>.' }

$name = Split-Path $Installer -Leaf
if ($name -notmatch '^Laststream-Setup-\d+\.\d+\.\d+\.exe$') { throw "Nom d'installeur inattendu : $name (attendu Laststream-Setup-X.Y.Z.exe)" }

$cmd = @('--yes', 'wrangler@4', 'r2', 'object', 'put', "$bucket/windows/$name", '--file', $Installer, '--content-type', 'application/vnd.microsoft.portable-executable', '--remote')
if ($DryRun) { Write-Host "npx $($cmd -join ' ')"; exit 0 }

npx @cmd
if ($LASTEXITCODE -ne 0) { throw "wrangler a echoue (code $LASTEXITCODE)" }
Write-Host "Publie : $publicBase/$name"
