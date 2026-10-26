import assert from 'node:assert/strict';
import { generateKeyPairSync, verify } from 'node:crypto';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { KEY_ENV, signBytes, signFile } from './sign-update.mjs';

// Paire generee a la volee : aucune cle n'est stockee dans le depot.
function newPair() {
  const { publicKey, privateKey } = generateKeyPairSync('ed25519');
  return { publicKey, pem: privateKey.export({ type: 'pkcs8', format: 'pem' }) };
}

const SCRIPT = fileURLToPath(new URL('./sign-update.mjs', import.meta.url));
const tmp = () => mkdtempSync(join(tmpdir(), 'sign-test-'));

test('la signature verifie sur les octets exacts et pas sur un fichier modifie', () => {
  const { publicKey, pem } = newPair();
  const data = Buffer.from('{"version":"1.0.18"}\n');
  const sig = Buffer.from(signBytes(data, pem), 'base64');
  assert.equal(sig.length, 64);
  assert.equal(verify(null, data, publicKey, sig), true);
  assert.equal(verify(null, Buffer.concat([data, Buffer.from(' ')]), publicKey, sig), false);
  assert.equal(verify(null, data, newPair().publicKey, sig), false);
});

test('signFile ecrit <fichier>.sig en base64', () => {
  const dir = tmp();
  try {
    const { publicKey, pem } = newPair();
    const file = join(dir, 'latest.json');
    writeFileSync(file, '{"a":1}');
    const out = signFile(file, pem);
    assert.equal(out, `${file}.sig`);
    assert.equal(verify(null, readFileSync(file), publicKey, Buffer.from(readFileSync(out, 'utf8'), 'base64')), true);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('cle absente, invalide ou non ed25519 : refus sans divulguer la cle', () => {
  assert.throws(() => signBytes(Buffer.from('x'), undefined), /absente/);
  assert.throws(() => signBytes(Buffer.from('x'), '  '), /absente/);
  const secret = 'SECRET-DO-NOT-LEAK';
  assert.throws(
    () => signBytes(Buffer.from('x'), `-----BEGIN PRIVATE KEY-----\n${secret}\n-----END PRIVATE KEY-----`),
    (error) => /PEM valide/.test(error.message) && !error.message.includes(secret),
  );
  const rsa = generateKeyPairSync('rsa', { modulusLength: 2048 }).privateKey.export({ type: 'pkcs8', format: 'pem' });
  assert.throws(() => signBytes(Buffer.from('x'), rsa), /ed25519/);
});

test('la ligne de commande lit la cle dans l environnement et ne l affiche pas', () => {
  const dir = tmp();
  try {
    const { publicKey, pem } = newPair();
    const file = join(dir, 'latest.json');
    writeFileSync(file, '{"b":2}');
    const ok = spawnSync(process.execPath, [SCRIPT, file], { env: { ...process.env, [KEY_ENV]: pem }, encoding: 'utf8' });
    assert.equal(ok.status, 0, ok.stderr);
    assert.ok(!ok.stdout.includes('PRIVATE') && !ok.stderr.includes('PRIVATE'));
    assert.equal(verify(null, readFileSync(file), publicKey, Buffer.from(readFileSync(`${file}.sig`, 'utf8'), 'base64')), true);

    const env = { ...process.env };
    delete env[KEY_ENV];
    const missing = spawnSync(process.execPath, [SCRIPT, file], { env, encoding: 'utf8' });
    assert.equal(missing.status, 1);
    assert.match(missing.stderr, /absente/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
