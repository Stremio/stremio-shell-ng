// Signe le flux de mise a jour Windows : ecrit <fichier>.sig, la signature ed25519 detachee (base64) des octets
// EXACTS du fichier (aucune canonicalisation). L'updater natif (src/laststream_app/updater.rs) telecharge latest.json
// et latest.json.sig et refuse la mise a jour si la signature ne correspond pas a sa cle publique embarquee.
//   LASTSTREAM_UPDATE_SIGNING_KEY="<PEM PKCS#8>" node scripts/sign-update.mjs latest.json
// La cle privee n'est lue que dans la variable d'environnement (jamais en argument, jamais affichee ni journalisee).
import { createPrivateKey, sign } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

export const KEY_ENV = 'LASTSTREAM_UPDATE_SIGNING_KEY';

/** Signature ed25519 de `data` (Buffer) avec la cle PEM PKCS#8 ; retourne le base64. */
export function signBytes(data, pem) {
  if (!pem || !pem.trim()) throw new Error(`Variable ${KEY_ENV} absente ou vide`);
  let key;
  try {
    key = createPrivateKey(pem);
  } catch {
    // Message volontairement generique : ne jamais relayer le contenu de la cle.
    throw new Error(`${KEY_ENV} n'est pas une cle privee PEM valide`);
  }
  if (key.asymmetricKeyType !== 'ed25519') throw new Error(`${KEY_ENV} doit etre une cle ed25519`);
  return sign(null, data, key).toString('base64');
}

/** Signe `path` et ecrit `<path>.sig` ; retourne le chemin de la signature. */
export function signFile(path, pem) {
  const signature = signBytes(readFileSync(path), pem);
  const out = `${path}.sig`;
  writeFileSync(out, signature);
  return out;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const files = process.argv.slice(2);
  if (files.length !== 1 || files[0].startsWith('-')) {
    console.error('Usage : LASTSTREAM_UPDATE_SIGNING_KEY=<PEM> node scripts/sign-update.mjs <fichier>');
    process.exit(2);
  }
  try {
    console.log(`Signature ecrite : ${signFile(files[0], process.env[KEY_ENV])}`);
  } catch (error) {
    console.error(`Signature impossible : ${error.message}`);
    process.exit(1);
  }
}
