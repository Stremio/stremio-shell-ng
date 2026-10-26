// Flux de version Windows (windows/latest.json) et controle de coherence des numeros de version.
//   node scripts/windows-manifest.mjs check
//   node scripts/windows-manifest.mjs build <installeur.exe> [--notes "texte"] [--out latest.json] [--base-url URL]
// Le schema reprend celui du latest.json Android (versionName, versionCode, url, sha256, size, notes) et ajoute
// `version`, `versionDesc` et `files[]`, que lit deja l'updater natif (src/laststream_app/updater.rs) : le meme fichier
// sert d'endpoint et de descripteur. Aucun secret ni reseau : le script ne fait que lire et hacher des fichiers.
import { createHash } from 'node:crypto';
import { createReadStream, readFileSync, statSync, writeFileSync } from 'node:fs';
import { basename, dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

export const DEFAULT_BASE_URL = 'https://laststream.gtux-prog.workers.dev/app/windows';
const INSTALLER_RE = /^Laststream-Setup-(\d+)\.(\d+)\.(\d+)\.exe$/;
const SEMVER_RE = /^(\d+)\.(\d+)\.(\d+)$/;

/** Extrait la version X.Y.Z du nom Laststream-Setup-X.Y.Z.exe ; leve une erreur sinon. */
export function parseInstallerName(name) {
  const m = INSTALLER_RE.exec(name);
  if (!m) throw new Error(`Nom d'installeur inattendu : ${name} (attendu Laststream-Setup-X.Y.Z.exe)`);
  return `${m[1]}.${m[2]}.${m[3]}`;
}

/** Entier croissant avec la version (major*1e6 + minor*1e3 + patch), comparable comme le versionCode Android. */
export function versionCode(version) {
  const m = SEMVER_RE.exec(version);
  if (!m) throw new Error(`Version invalide : ${version} (attendu X.Y.Z)`);
  const [major, minor, patch] = [Number(m[1]), Number(m[2]), Number(m[3])];
  if (minor > 999 || patch > 999) throw new Error(`Version hors plage : ${version} (minor et patch <= 999)`);
  return major * 1_000_000 + minor * 1_000 + patch;
}

/** Construit le flux a partir du nom, de la somme SHA-256 (64 hex), de la taille et des notes. */
export function buildManifest({ name, sha256, size, notes = '', publishedAt, baseUrl = DEFAULT_BASE_URL }) {
  const version = parseInstallerName(name);
  if (!/^[0-9a-f]{64}$/.test(sha256)) throw new Error('sha256 invalide (64 caracteres hexadecimaux minuscules attendus)');
  if (!Number.isInteger(size) || size <= 0) throw new Error('size invalide (entier > 0 attendu)');
  const base = baseUrl.replace(/\/+$/, '');
  if (!/^https:\/\//.test(base)) throw new Error('baseUrl doit etre en https');
  const url = `${base}/${name}`;
  return {
    version,
    versionName: version,
    versionCode: versionCode(version),
    publishedAt: publishedAt ?? new Date().toISOString(),
    url,
    sha256,
    size,
    notes,
    versionDesc: `${base}/latest.json`,
    files: [{ name, url, checksum: sha256, os: 'windows' }],
  };
}

export function sha256File(path) {
  return new Promise((resolve, reject) => {
    const hash = createHash('sha256');
    createReadStream(path).on('data', (c) => hash.update(c)).on('error', reject).on('end', () => resolve(hash.digest('hex')));
  });
}

const packageSection = (cargoToml) => /\[package\]([\s\S]*?)(?:\n\[|$)/.exec(cargoToml)?.[1] ?? '';

export function readCargoVersion(cargoToml) {
  const m = /^version\s*=\s*"([^"]+)"/m.exec(packageSection(cargoToml));
  if (!m) throw new Error('Version introuvable dans Cargo.toml');
  return m[1];
}

export function readIssVersion(iss) {
  const m = /^#define\s+MyAppVersion\s+"([^"]+)"/m.exec(iss);
  if (!m) throw new Error('MyAppVersion introuvable dans setup/Laststream.iss');
  return m[1];
}

/** Retourne la liste des incoherences entre Cargo.toml, Cargo.lock et Laststream.iss (vide si tout concorde). */
export function checkVersions({ cargoToml, cargoLock, iss }) {
  const cargo = readCargoVersion(cargoToml);
  const problems = [];
  const issVersion = readIssVersion(iss);
  if (issVersion !== cargo) problems.push(`Laststream.iss (${issVersion}) differe de Cargo.toml (${cargo})`);
  const name = /^name\s*=\s*"([^"]+)"/m.exec(packageSection(cargoToml))?.[1];
  const lock = name && new RegExp(`\\[\\[package\\]\\]\\r?\\nname = "${name}"\\r?\\nversion = "([^"]+)"`).exec(cargoLock);
  if (!lock) problems.push(`Entree ${name ?? '(nom du paquet)'} introuvable dans Cargo.lock`);
  else if (lock[1] !== cargo) problems.push(`Cargo.lock (${lock[1]}) differe de Cargo.toml (${cargo})`);
  try {
    versionCode(cargo);
  } catch (e) {
    problems.push(e.message);
  }
  return problems;
}

export const arg = (args, flag) => {
  const i = args.indexOf(flag);
  const value = i >= 0 ? args[i + 1] : undefined;
  // Windows PowerShell 5.1 supprime un argument vide : `--notes --out f` ne doit pas lire `--out` comme notes.
  return value?.startsWith('--') ? undefined : value;
};

async function main(argv) {
  const root = join(dirname(fileURLToPath(import.meta.url)), '..');
  const [cmd, ...rest] = argv;
  if (cmd === 'check') {
    const problems = checkVersions({
      cargoToml: readFileSync(join(root, 'Cargo.toml'), 'utf8'),
      cargoLock: readFileSync(join(root, 'Cargo.lock'), 'utf8'),
      iss: readFileSync(join(root, 'setup', 'Laststream.iss'), 'utf8'),
    });
    if (problems.length) {
      for (const p of problems) console.error(`Version : ${p}`);
      return 1;
    }
    console.log('Versions coherentes.');
    return 0;
  }
  if (cmd === 'build') {
    const installer = rest[0]?.startsWith('--') ? undefined : rest[0];
    if (!installer) throw new Error('Usage : build <installeur.exe> [--notes texte] [--out fichier] [--base-url url]');
    const manifest = buildManifest({
      name: basename(installer),
      sha256: await sha256File(installer),
      size: statSync(installer).size,
      notes: arg(rest, '--notes') ?? '',
      baseUrl: arg(rest, '--base-url') ?? DEFAULT_BASE_URL,
    });
    const json = `${JSON.stringify(manifest, null, 2)}\n`;
    const out = arg(rest, '--out');
    if (out) writeFileSync(out, json);
    else process.stdout.write(json);
    return 0;
  }
  console.error('Usage : windows-manifest.mjs check | build <installeur.exe> [options]');
  return 2;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main(process.argv.slice(2)).then(
    (code) => process.exit(code),
    (e) => {
      console.error(e.message);
      process.exit(1);
    },
  );
}
