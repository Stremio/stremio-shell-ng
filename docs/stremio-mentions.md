# Mentions restantes de « Stremio »

LAST-171 : le shell ne porte plus la marque Stremio (modules, chaînes visibles, crate, scripts, CI).
`git grep -i -e stremio -e strem.io` ne renvoie plus que les éléments ci-dessous, qu'on ne peut pas ou ne doit pas renommer.

| Où | Pourquoi on le garde |
|---|---|
| `src/laststream_app/laststream_server/server.rs` et `window_settings.rs` : dossiers `%LOCALAPPDATA%\Stremio` et `%APPDATA%\Stremio` | Chemins hérités des anciennes installations. Au démarrage, leurs réglages sont copiés vers `Laststream` si la cible n'existe pas encore (copie, pas déplacement : une vraie installation de Stremio peut partager ces dossiers). Tests unitaires dans les deux fichiers. |
| `stremio-cache` (nom du dossier de cache, dans le test de `server.rs`) | Nom imposé par `server.js` (serveur de streaming amont embarqué), qui crée et lit ce dossier. |
| `native-windows-gui` (`Cargo.toml`, `Cargo.lock`) : dépôt `github.com/Stremio/native-windows-gui` | Dépendance tierce (fork) ; l'URL du dépôt ne peut pas changer. |
| `server.js` (41 lignes avec « stremio », 8 avec `strem.io`) | Serveur de streaming amont embarqué, bundle tiers non modifié : noms de paquets, URLs GitHub, URLs `strem.io` (Cinemeta, API, origine CORS, `local.strem.io`), en-têtes, licence. |
| `mpv-x64/mpv.exp`, `mpv-arm64/libmpv-2.exp` | Fichiers d'export binaires de la libmpv compilée par l'amont. |
| Remote git `upstream` | Dépôt amont `Stremio/stremio-shell-ng`, gardé pour suivre les correctifs. |
| `docs/stremio-mentions.md` et son lien dans `README.md` | Ce fichier, et la phrase du README qui y renvoie. |
