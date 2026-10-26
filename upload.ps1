# Publie l'installeur Windows de Laststream dans le bucket R2 laststream-app-files (cle windows/<nom>), puis le flux
# de version windows/latest.json (version, notes, URL, taille, SHA-256 ; schema : scripts/windows-manifest.mjs).
# Le flux est ecrit apres l'installeur : une version annoncee est toujours telechargeable.
# Le Worker laststream-edge sert les deux sur /app/windows/ (voir appFile() dans laststream-edge/src/index.js).
# Prerequis : `wrangler login` deja fait, Node.js. Aucun secret n'est lu ni ecrit par ce script.
#   .\upload.ps1                       # prend le Laststream-Setup-X.Y.Z.exe le plus recent de ..\dist\installer
#   .\upload.ps1 -Installer <fichier>  # installeur precis
#   .\upload.ps1 -Notes "Correctifs"   # notes de version du flux
#   .\upload.ps1 -DryRun               # affiche les commandes et le flux sans rien envoyer
# Signature : aucun certificat de signature de code (Authenticode) n'est utilise (SmartScreen peut alerter). Le flux est signe
# en ed25519 (scripts/sign-update.mjs) : latest.json.sig est publie a cote de latest.json, et l'updater natif refuse toute
# mise a jour dont la signature ne correspond pas a sa cle publique embarquee (LAST-88). La cle privee PEM est lue dans la
# variable d'environnement LASTSTREAM_UPDATE_SIGNING_KEY (secret GitHub du meme nom), jamais en argument.
param(
    [string]$Installer,
    [string]$Notes = '',
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
$bucket = 'laststream-app-files'
$publicBase = 'https://laststream.gtux-prog.workers.dev/app/windows'
$manifestTool = Join-Path $PSScriptRoot 'scripts\windows-manifest.mjs'

if (-not $Installer) {
    $Installer = Get-ChildItem (Join-Path $PSScriptRoot '..\dist\installer') -Filter 'Laststream-Setup-*.exe' -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1 -ExpandProperty FullName
}
if (-not $Installer -or -not (Test-Path $Installer)) { throw 'Installeur introuvable : lancer build.ps1 ou passer -Installer <fichier>.' }

$name = Split-Path $Installer -Leaf
if ($name -notmatch '^Laststream-Setup-(\d+\.\d+\.\d+)\.exe$') { throw "Nom d'installeur inattendu : $name (attendu Laststream-Setup-X.Y.Z.exe)" }
$version = $Matches[1]

# Cargo.toml, Cargo.lock et Laststream.iss doivent porter la meme version, et l'installeur aussi.
node $manifestTool check
if ($LASTEXITCODE -ne 0) { throw 'Versions incoherentes (Cargo.toml, Cargo.lock, Laststream.iss).' }
$iss = Select-String -Path (Join-Path $PSScriptRoot 'setup\Laststream.iss') -Pattern '^#define MyAppVersion "([^"]+)"' |
    ForEach-Object { $_.Matches[0].Groups[1].Value }
if ($iss -ne $version) { throw "L'installeur est en $version mais setup\Laststream.iss est en $iss : reconstruire l'installeur." }

$manifest = Join-Path ([IO.Path]::GetTempPath()) "laststream-latest-$version.json"
node $manifestTool build $Installer --notes $Notes --out $manifest --base-url $publicBase
if ($LASTEXITCODE -ne 0) { throw 'Generation du flux latest.json echouee.' }

# Le flux est signe avant tout envoi : sans cle, rien n'est publie (une mise a jour non signee serait refusee par l'updater).
$feedSig = "$manifest.sig"
if ($DryRun -and -not $env:LASTSTREAM_UPDATE_SIGNING_KEY) {
    Write-Host "(DryRun) LASTSTREAM_UPDATE_SIGNING_KEY absente : flux non signe, rien n'est envoye."
} else {
    if (-not $env:LASTSTREAM_UPDATE_SIGNING_KEY) { throw 'LASTSTREAM_UPDATE_SIGNING_KEY absente : impossible de signer latest.json.' }
    # Une signature restee d'une publication precedente de la meme version ne doit jamais etre envoyee.
    Remove-Item $feedSig -ErrorAction SilentlyContinue
    node (Join-Path $PSScriptRoot 'scripts/sign-update.mjs') $manifest
    if ($LASTEXITCODE -ne 0) { throw 'Signature de latest.json echouee.' }
    if (-not (Test-Path $feedSig)) { throw "Signature de latest.json absente apres signature : $feedSig" }
}

$put = @('--yes', 'wrangler@4', 'r2', 'object', 'put')
$putInstaller = $put + @("$bucket/windows/$name", '--file', $Installer, '--content-type', 'application/vnd.microsoft.portable-executable', '--remote')
$putFeed = $put + @("$bucket/windows/latest.json", '--file', $manifest, '--content-type', 'application/json', '--cache-control', 'public, max-age=300', '--remote')
$putFeedSig = $put + @("$bucket/windows/latest.json.sig", '--file', $feedSig, '--content-type', 'text/plain', '--cache-control', 'public, max-age=300', '--remote')
if ($DryRun) {
    Write-Host "npx $($putInstaller -join ' ')"
    Write-Host "npx $($putFeedSig -join ' ')"
    Write-Host "npx $($putFeed -join ' ')"
    Get-Content $manifest
    exit 0
}

npx @putInstaller
if ($LASTEXITCODE -ne 0) { throw "wrangler a echoue sur l'installeur (code $LASTEXITCODE)" }
# La signature d'abord : latest.json n'apparait qu'une fois sa signature en place (sinon les clients refuseraient la mise a jour).
npx @putFeedSig
if ($LASTEXITCODE -ne 0) { throw "wrangler a echoue sur latest.json.sig (code $LASTEXITCODE)" }
npx @putFeed
if ($LASTEXITCODE -ne 0) { throw "wrangler a echoue sur latest.json (code $LASTEXITCODE)" }
Write-Host "Publie : $publicBase/$name"
Write-Host "Flux   : $publicBase/latest.json (+ latest.json.sig)"
