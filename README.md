## Laststream : coque Windows

A Windows-only shell using WebView2 and MPV (crate `laststream-shell-ng` ;
l'exécutable livré s'appelle `laststream.exe`)

Goals:
* Performance
* Reliability
* Easy to ship

In all three, this architecture excels a Qt-based shell: it is about 2-5x more efficient depending on the use case, as it allows MPV to render directly in the window through it's optimal video output rather than using libmpv to integrate with Qt.

This is due to Qt having a complex rendering pipeline involving ANGLE and multiple levels of composing and drawing to textures, which inhibits full HW acceleration.

Meanwhile in this setup MPV uses whichever pipeline it considers to be optimal (like the mpv desktop app), which is normally d3d11, allowing full HW acceleration.

For web rendering, we use the native WebView2, which is Chromium based but shipped as a part of Windows 10: therefore we do not need to ship our own "distribution" of Chromium.

Finally, this should be a lot more reliable as it uses a much simpler and more native overall architecture.

## Native ASS subtitles

The WebUI handshake advertises `nativeAssSubtitles=true` after the existing GPU
capability. The player accepts these subtitle commands:

```json
["mpv-command", ["sub-add", "https://example.com/subtitles.ass", "auto", "English", "eng"]]
["mpv-command", ["sub-remove", "7"]]
```

Subtitle loading runs on a dedicated mpv client with one asynchronous command in
flight. `loadfile` and `stop` cancel and drain subtitle work before changing media.
Subtitle commands do not reset video readiness. The WebUI matches `track-list`
titles to the current request before setting `sid`, preventing late loads from
changing the user's selection.

mpv handles fonts embedded in the media or subtitle file and installed system
fonts. Subtitles with separate font URLs stay in the WebUI libass renderer.

## Publier une version Windows

L'installeur se construit en local (le dossier `dist\laststream-desktop` contient l'UI, Node, ffmpeg et lastAddon, qui ne sont pas dans ce dépôt) puis se publie en une commande.

1. Aligner la version dans `Cargo.toml`, `Cargo.lock` et `setup/Laststream.iss` (`MyAppVersion`). La CI refuse un écart (`node scripts/windows-manifest.mjs check`). `Cargo.toml` fait foi pour la version exposée à l'interface.
2. `.\build.ps1` : compile en release puis construit `dist\installer\Laststream-Setup-X.Y.Z.exe` avec ISCC.
3. `.\upload.ps1 -Notes "…"` (`-DryRun` pour simuler) : envoie l'installeur dans R2 (`laststream-app-files`, clé `windows/`), puis le flux `windows/latest.json`. Le Worker `laststream-edge` les sert sur `https://laststream.gtux-prog.workers.dev/app/windows/`.

Le flux (`scripts/windows-manifest.mjs`) suit le schéma du `latest.json` Android (`versionName`, `versionCode`, `url`, `sha256`, `size`, `notes`) et ajoute `version`, `publishedAt`, `versionDesc` et `files[]`, le format que lit déjà l'updater natif (`--autoupdater-endpoint`) : le même fichier est l'endpoint et le descripteur. `versionCode` vaut `major*1000000 + minor*1000 + patch`. L'updater natif compare le SHA-256 au fichier téléchargé avant de lancer l'installeur ; il lit ce flux par défaut (ODY-258) : `--autoupdater-endpoint <URL>` le remplace, `--no-autoupdater` le désactive (aucun appel réseau), comme `--development`. Si une version plus récente est trouvée, l'installeur est téléchargé puis vérifié ; l'installation silencieuse (`/SILENT`) ne démarre qu'après le clic de l'utilisateur sur la notification de mise à jour. L'installeur est écrit dans un fichier `.part` puis renommé après la vérification, et une version déjà téléchargée n'est pas retéléchargée. Le SHA-256 vient du même flux que l'URL, et ce flux est authentifié (LAST-88) : `latest.json` est signé en ed25519 par `scripts/sign-update.mjs` (signature détachée en base64 des octets exacts, publiée en `latest.json.sig` à côté), et l'updater télécharge les deux puis vérifie la signature avec la clé publique embarquée (`AUTOUPDATER_PUBLIC_KEY_HEX` dans `constants.rs`) AVANT de lire le moindre champ : sans `.sig`, avec une signature invalide ou une clé mal formée, la mise à jour est refusée, rien n'est installé et le refus est journalisé. Le flux, le descripteur et l'installeur ne sont téléchargés que sur `https` et sur les hôtes de `AUTOUPDATER_ALLOWED_HOSTS` (`laststream.gtux-prog.workers.dev`) ; 3 redirections au plus, chacune contrôlée de la même façon ; délais de connexion (10 s), de flux (30 s) et d'installeur (30 min) ; le fichier `.part` est supprimé après tout échec (réseau coupé, SHA-256 faux, signature refusée).

Publication signée : la clé privée PEM (PKCS#8) est le secret GitHub `LASTSTREAM_UPDATE_SIGNING_KEY`. `upload.ps1` signe le flux (`node scripts/sign-update.mjs <latest.json>`, clé lue dans la variable d'environnement du même nom, jamais en argument) puis téléverse `latest.json.sig` avant `latest.json`. À la main : signer `latest.json`, puis téléverser `latest.json` et `latest.json.sig`. Changer de clé exige de publier un shell avec la nouvelle clé publique avant de signer avec elle.

Signature : aucun certificat de signature de code n'est utilisé à ce jour, SmartScreen peut donc alerter au premier lancement. L'intégrité repose sur le SHA-256 du flux. Pour signer, définir l'outil `laststreamsign` dans Inno Setup puis compiler avec `/DSIGN`.

## Release

Une version Windows se publie par tag, en quatre étapes. Le workflow `.github/workflows/release.yml` ne se lance qu'à la main (`workflow_dispatch`, entrée `tag`) : un push de tag ne le déclenche pas, pour qu'il ne démarre pas avant le staging. Il construit l'installeur et publie la release ; les dépendances des dépôts voisins (`laststream-app`, `lastaddon`) sont préparées en local par `scripts/stage-release-deps.ps1`, sans jeton inter-dépôts.

1. **Tag** : aligner la version dans `Cargo.toml`, `Cargo.lock` et `setup/Laststream.iss`, pousser sur `main`, puis `git tag vX.Y.Z` et `git push origin vX.Y.Z`. Le workflow refuse un tag qui ne correspond pas à la version.
2. **Dépendances** (local, depuis le dépôt shell) : `.\scripts\stage-release-deps.ps1 -Version X.Y.Z` (`-DryRun` pour simuler, sans rien publier). Il exige `..\laststream-app` et `..\lastaddon` propres (fichiers suivis), sur `main` et à jour de `origin/main`, puis produit `ui.zip` (build de laststream-app), `lastaddon.zip` (`lastaddon.cjs` et `node.exe`) et `deps.json` (commits des deux dépôts, SHA-256). Il crée la release brouillon `vX.Y.Z` et y dépose les trois fichiers.
3. **Workflow** (`gh workflow run release.yml -f tag=vX.Y.Z`, ou « Run workflow » dans l'onglet Actions) : l'installeur `Laststream-Setup-X.Y.Z.exe`, son `.sha256`, `latest.json` et `latest.json.sig` (signature ed25519, secret `LASTSTREAM_UPDATE_SIGNING_KEY`) sont déposés sur la release, puis la release passe de brouillon à publiée. Le workflow vérifie les SHA-256 contre `deps.json` et échoue si les archives manquent (« lancer scripts/stage-release-deps.ps1 d'abord »). Le format du tag est vérifié avant tout téléchargement.
4. **R2** : téléversement à la main, depuis un poste avec `wrangler login` et `LASTSTREAM_UPDATE_SIGNING_KEY` : `.\upload.ps1 -Installer <Laststream-Setup-X.Y.Z.exe téléchargé> -Notes "…"`. Aucun jeton Cloudflare n'est dans le workflow.

Limites connues :
- `node.exe` est une copie locale (Node.js v22.23.3 win-x64) : aucun dépôt ne le fournit, son origine n'est pas tracée ; sa version et son SHA-256 figurent dans `deps.json` ;
- pas d'Authenticode (décision produit) : SmartScreen peut alerter ;
- `latest.json` pointe vers l'URL R2, qui ne répond qu'après l'étape 4.

Binaires volumineux : le dépôt contient déjà des binaires (`bin/`, `bin-arm64/`, `mpv-x64/`, `mpv-arm64/`, `libmpv-2_*.zip`, `server.js`). Leur historique n'est pas réécrit. Ne plus en ajouter : les bundles de build vont dans `dist/`, ignoré par git.

## Mentions de l'amont

Les références restantes à l'ancien projet amont sont listées et justifiées dans [docs/stremio-mentions.md](docs/stremio-mentions.md).
