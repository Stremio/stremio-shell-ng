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

Le flux (`scripts/windows-manifest.mjs`) suit le schéma du `latest.json` Android (`versionName`, `versionCode`, `url`, `sha256`, `size`, `notes`) et ajoute `version`, `publishedAt`, `versionDesc` et `files[]`, le format que lit déjà l'updater natif (`--autoupdater-endpoint`) : le même fichier est l'endpoint et le descripteur. `versionCode` vaut `major*1000000 + minor*1000 + patch`. L'updater natif compare le SHA-256 au fichier téléchargé avant de lancer l'installeur ; il lit ce flux par défaut (ODY-258) : `--autoupdater-endpoint <URL>` le remplace, `--no-autoupdater` le désactive (aucun appel réseau), comme `--development`. Si une version plus récente est trouvée, l'installeur est téléchargé puis vérifié ; l'installation silencieuse (`/SILENT`) ne démarre qu'après le clic de l'utilisateur sur la notification de mise à jour. L'installeur est écrit dans un fichier `.part` puis renommé après la vérification, et une version déjà téléchargée n'est pas retéléchargée. Le SHA-256 vient du même flux que l'URL : il protège de la corruption, pas d'une compromission du Worker ou du bucket (une signature du flux reste à faire).

Signature : aucun certificat de signature de code n'est utilisé à ce jour, SmartScreen peut donc alerter au premier lancement. L'intégrité repose sur le SHA-256 du flux. Pour signer, définir l'outil `laststreamsign` dans Inno Setup puis compiler avec `/DSIGN`.

## Mentions de l'amont

Les références restantes à l'ancien projet amont sont listées et justifiées dans [docs/stremio-mentions.md](docs/stremio-mentions.md).
