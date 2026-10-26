#Requires -Version 7.0
<#
.SYNOPSIS
  Prepare les archives de dependances d'une version (ui.zip, lastaddon.zip, deps.json) et les depose sur la release brouillon du tag.

.DESCRIPTION
  A lancer en local par le mainteneur, depuis ce depot (checkout principal ou worktree). Les depots voisins sont lus dans
  l'espace de travail, parent du checkout principal : ..\laststream-app et ..\lastaddon. Aucun jeton inter-depots : ni ici,
  ni dans release.yml, qui ne lit que la release du tag.

  1. Verifie que laststream-app et lastaddon sont propres, sur main, et egaux a origin/main (git fetch).
  2. ui.zip : npm run build dans laststream-app ; le contenu de dist\ est range sous ui\.
  3. lastaddon.zip : npm run bundle dans lastaddon (dist-bundle\lastaddon.cjs) et node.exe, ranges sous lastaddon\.
  4. deps.json : commits des deux depots, SHA-256 et taille des archives, version et SHA-256 de node.exe.
  5. Cree la release brouillon vX.Y.Z si elle est absente (gh release create --draft --verify-tag), puis televerse les 3 fichiers (--clobber).

  node.exe : copie locale de dist\laststream-desktop\lastaddon\node.exe (Node.js v22.23.3 win-x64 selon deps.json).
  Aucun depot ne le fournit et son origine n'est pas tracee : le script le prend tel quel et enregistre sa version et son SHA-256.

  Le tag vX.Y.Z doit deja etre pousse sur origin (--verify-tag).

.PARAMETER Version
  X.Y.Z. Doit correspondre a Cargo.toml de ce depot ; le tag est vX.Y.Z. Avec -DryRun, un ecart n'est qu'un avertissement.

.PARAMETER DryRun
  Fait tout, sauf l'appel a gh release (rien n'est envoye).

.EXAMPLE
  .\scripts\stage-release-deps.ps1 -Version 1.0.25 -DryRun
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version,

    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
$tag = "v$Version"

# Checkout principal du depot shell (un worktree pointe vers le depot principal par git-common-dir), puis espace de travail.
$shellRoot = Split-Path -Parent (git -C $PSScriptRoot rev-parse --path-format=absolute --git-common-dir)
$workspace = Split-Path -Parent $shellRoot
$appDir = Join-Path $workspace 'laststream-app'
$addonDir = Join-Path $workspace 'lastaddon'
$nodeExe = Join-Path $workspace 'dist\laststream-desktop\lastaddon\node.exe'
$stage = Join-Path $shellRoot "dist\stage-$tag"

$cargoToml = Get-Content -LiteralPath (Join-Path (Split-Path -Parent $PSScriptRoot) 'Cargo.toml') -Raw
$cargoVersion = [regex]::Match($cargoToml, '(?m)^version = "([^"]+)"').Groups[1].Value
if ($cargoVersion -ne $Version) {
    $msg = "Cargo.toml de ce depot porte $cargoVersion, pas $Version."
    # En DryRun, l'ecart n'est qu'un avertissement (rien n'est publie) ; hors DryRun il bloque.
    if ($DryRun) { Write-Warning $msg } else { throw $msg }
}

# 1. Depots voisins : propres, sur main, egaux a origin/main. Tous les problemes sont listes avant d'abandonner.
$problems = [System.Collections.Generic.List[string]]::new()
$commits = [ordered]@{}
foreach ($dir in @($appDir, $addonDir)) {
    $name = Split-Path -Leaf $dir
    if (-not (Test-Path -LiteralPath (Join-Path $dir '.git'))) { $problems.Add("$name : depot absent ($dir)"); continue }
    # Fichiers suivis seulement : les non suivis (.claude/, nul) ne bloquent pas.
    $dirty = @(git -C $dir status --porcelain --untracked-files=no)
    if ($dirty.Count -gt 0) { $problems.Add("$name : arbre non propre ($($dirty.Count) entree(s) : $($dirty -join ' ; '))") }
    $branch = git -C $dir rev-parse --abbrev-ref HEAD
    if ($branch -ne 'main') { $problems.Add("$name : branche $branch, attendu main") }
    git -C $dir fetch --quiet origin main
    if ($LASTEXITCODE -ne 0) { $problems.Add("$name : git fetch origin main a echoue") }
    $head = git -C $dir rev-parse HEAD
    $remote = git -C $dir rev-parse origin/main
    if ($head -ne $remote) { $problems.Add("$name : HEAD $($head.Substring(0, 7)) different de origin/main $($remote.Substring(0, 7))") }
    $commits[$name] = $head
}
if (-not (Test-Path -LiteralPath $nodeExe)) { $problems.Add("node.exe absent : $nodeExe") }
if ($problems.Count -gt 0) {
    $problems | ForEach-Object { Write-Host "ERREUR : $_" -ForegroundColor Red }
    throw "Prerequis non prets ($($problems.Count) probleme(s)) : rien n'a ete construit."
}

# 2. ui.zip : build de laststream-app, dist\ range sous ui\.
Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
$uiStage = Join-Path $stage 'ui'
$addonStage = Join-Path $stage 'lastaddon'
New-Item -ItemType Directory -Force $uiStage, $addonStage | Out-Null

Push-Location $appDir
try {
    npm run build
    if ($LASTEXITCODE -ne 0) { throw "npm run build (laststream-app) a echoue (code $LASTEXITCODE)" }
} finally { Pop-Location }
if (-not (Test-Path -LiteralPath (Join-Path $appDir 'dist\index.html'))) { throw 'laststream-app\dist\index.html absent apres npm run build' }
Copy-Item -Path (Join-Path $appDir 'dist\*') -Destination $uiStage -Recurse -Force

# 3. lastaddon.zip : bundle mono-fichier (lastaddon.cjs) et node.exe.
Push-Location $addonDir
try {
    npm run bundle
    if ($LASTEXITCODE -ne 0) { throw "npm run bundle (lastaddon) a echoue (code $LASTEXITCODE)" }
} finally { Pop-Location }
$cjs = Join-Path $addonDir 'dist-bundle\lastaddon.cjs'
if (-not (Test-Path -LiteralPath $cjs)) { throw "lastaddon.cjs absent apres npm run bundle : $cjs" }
Copy-Item -LiteralPath $cjs -Destination $addonStage
Copy-Item -LiteralPath $nodeExe -Destination $addonStage
$nodeVersion = (& $nodeExe --version).Trim()

Add-Type -AssemblyName System.IO.Compression.FileSystem
$uiZip = Join-Path $stage 'ui.zip'
$addonZip = Join-Path $stage 'lastaddon.zip'
$level = [System.IO.Compression.CompressionLevel]::Optimal
# includeBaseDirectory : les entrees commencent par ui/ et lastaddon/, pour un extract a la racine de dist\laststream-desktop.
[System.IO.Compression.ZipFile]::CreateFromDirectory($uiStage, $uiZip, $level, $true)
[System.IO.Compression.ZipFile]::CreateFromDirectory($addonStage, $addonZip, $level, $true)

# 4. deps.json
function Get-Sha256([string]$path) { (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() }
$deps = [ordered]@{
    version = $Version
    tag     = $tag
    builtAt = (Get-Date).ToUniversalTime().ToString('o')
    repos   = [ordered]@{
        'laststream-app' = [ordered]@{ branch = 'main'; commit = $commits['laststream-app'] }
        'lastaddon'      = [ordered]@{ branch = 'main'; commit = $commits['lastaddon'] }
    }
    files   = [ordered]@{
        'ui.zip'        = [ordered]@{ sha256 = (Get-Sha256 $uiZip); size = (Get-Item -LiteralPath $uiZip).Length }
        'lastaddon.zip' = [ordered]@{ sha256 = (Get-Sha256 $addonZip); size = (Get-Item -LiteralPath $addonZip).Length }
    }
    nodeExe = [ordered]@{
        version = $nodeVersion
        sha256  = (Get-Sha256 $nodeExe)
        origin  = 'copie locale de dist\laststream-desktop\lastaddon\node.exe (aucun depot ne le fournit, origine non tracee)'
    }
}
$depsPath = Join-Path $stage 'deps.json'
$deps | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $depsPath -Encoding utf8
Write-Host "Archives prêtes dans $stage :"
Get-Content -LiteralPath $depsPath

if ($DryRun) {
    Write-Host 'DryRun : aucun appel a gh release.'
    exit 0
}

# 5. Release brouillon, puis televersement (remplace les fichiers homonymes).
gh release view $tag *> $null
if ($LASTEXITCODE -ne 0) {
    gh release create $tag --verify-tag --draft --title "Laststream $tag" --notes 'Brouillon : dependances de release (ui.zip, lastaddon.zip, deps.json). Publication par le workflow release.yml.'
    if ($LASTEXITCODE -ne 0) { throw "gh release create $tag a echoue (le tag doit etre pousse sur origin)" }
}
gh release upload $tag $uiZip $addonZip $depsPath --clobber
if ($LASTEXITCODE -ne 0) { throw "gh release upload a echoue (code $LASTEXITCODE)" }
Write-Host "Brouillon $tag : ui.zip, lastaddon.zip et deps.json televerses."
