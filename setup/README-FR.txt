LASTSTREAM - application de bureau (Windows)
=============================================

Contenu du dossier
------------------
  laststream.exe            Coque Windows (WebView2 + mpv) qui lance les services locaux
                            et ouvre l'application Laststream.
  libmpv-2.dll              Lecteur mpv (lecture native, requis par la coque).
  laststream-runtime.exe     Runtime Node 18 utilise pour lancer le serveur de streaming.
  server.js                 Serveur de streaming local (demarre par la coque).
  ffmpeg.exe, ffprobe.exe   Transcodage utilise par le serveur de streaming.
  *.dll                     Bibliotheques ffmpeg et runtime (ne pas deplacer).
  lastaddon\node.exe        Node.js 22.23.3 LTS (portable, requis par lastAddon : node:sqlite).
  lastaddon\lastaddon.cjs   Service local Laststream (lastAddon, bundle unique).
  ui\                       Interface Laststream (build de laststream-app), servie par
                            lastAddon sur http://127.0.0.1:<port>/.

Lancement
---------
Double-cliquer sur laststream.exe. Au demarrage, la coque :
  1. choisit le premier port libre a partir de 7000 (aucune connexion ni ecoute active) ;
  2. lance lastaddon\node.exe lastaddon\lastaddon.cjs avec ce port (variable PORT) et
     attend la ligne "LASTADDON_READY" pendant 20 secondes ;
  3. ouvre l'interface Laststream sur http://127.0.0.1:<port>/.
Si le service ne demarre pas, une fenetre d'erreur propose Reessayer ou Annuler.
Si le service s'arrete pendant l'utilisation, il est relance une fois automatiquement.
Le service est arrete avec la coque.

Options utiles
--------------
  --webui-url=<url>   Charge une autre interface web, a la place de celle de Laststream.
  --development       Desactive le serveur de streaming et le service local, et charge
                      http://127.0.0.1:11470.

Donnees
-------
Les donnees du service local sont stockees dans :
  %APPDATA%\Laststream\lastaddon
Le fichier .env de ce dossier contient le secret de chiffrement : le conserver pour que
les donnees restent lisibles apres une mise a jour.

Import au premier lancement
---------------------------
Si ce dossier ne contient encore ni laststream.db ni .env, la coque copie les donnees de
l'instance lastAddon de developpement presente sur le poste, s'il y en a une : base
laststream.db avec ses fichiers WAL/SHM, et .env (secret). Compte, profils, bibliotheque et
progression sont conserves, et les configurations chiffrees restent valides. Rien n'est
ecrase : un second lancement n'importe rien. Pour un import propre, arreter l'instance de
developpement avant le premier lancement (la base est en mode WAL).
  LASTADDON_IMPORT_FROM=<dossier>  autre source (dossier contenant data\ et .env)
  LASTADDON_IMPORT_FROM=           (vide) desactive l'import
Seul le message "import effectue" est ecrit dans la sortie standard de la coque ; le contenu
n'est jamais journalise.

Desinstallation
---------------
Supprimer le dossier. Les donnees restent dans %APPDATA%\Laststream si on veut les conserver.
