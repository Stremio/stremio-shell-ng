import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  arg, buildManifest, checkVersions, parseInstallerName, readCargoVersion, readIssVersion, versionCode,
} from './windows-manifest.mjs';

const SHA = 'a'.repeat(64);

test('parseInstallerName accepte X.Y.Z et refuse le reste', () => {
  assert.equal(parseInstallerName('Laststream-Setup-1.0.17.exe'), '1.0.17');
  assert.throws(() => parseInstallerName('Stremio-Setup-1.0.17.exe'));
  assert.throws(() => parseInstallerName('Laststream-Setup-1.0.exe'));
  assert.throws(() => parseInstallerName('../Laststream-Setup-1.0.17.exe'));
});

test('versionCode croit avec la version', () => {
  assert.equal(versionCode('1.0.17'), 1000017);
  assert.ok(versionCode('1.0.18') > versionCode('1.0.17'));
  assert.ok(versionCode('1.1.0') > versionCode('1.0.999'));
  assert.ok(versionCode('2.0.0') > versionCode('1.999.999'));
  assert.throws(() => versionCode('1.0'));
  assert.throws(() => versionCode('1.0.1000'));
});

test('buildManifest produit le schema Android + descripteur de l updater natif', () => {
  const m = buildManifest({
    name: 'Laststream-Setup-1.0.17.exe', sha256: SHA, size: 123, notes: 'n', publishedAt: '2026-10-23T00:00:00.000Z',
    baseUrl: 'https://exemple.test/app/windows/',
  });
  assert.equal(m.version, '1.0.17');
  assert.equal(m.versionName, '1.0.17');
  assert.equal(m.versionCode, 1000017);
  assert.equal(m.url, 'https://exemple.test/app/windows/Laststream-Setup-1.0.17.exe');
  assert.equal(m.sha256, SHA);
  assert.equal(m.size, 123);
  assert.equal(m.notes, 'n');
  assert.equal(m.versionDesc, 'https://exemple.test/app/windows/latest.json');
  assert.deepEqual(m.files, [{ name: 'Laststream-Setup-1.0.17.exe', url: m.url, checksum: SHA, os: 'windows' }]);
});

test('buildManifest refuse sha256, taille ou base invalides', () => {
  const ok = { name: 'Laststream-Setup-1.0.17.exe', sha256: SHA, size: 1 };
  assert.throws(() => buildManifest({ ...ok, sha256: 'ABC' }));
  assert.throws(() => buildManifest({ ...ok, sha256: SHA.toUpperCase() }));
  assert.throws(() => buildManifest({ ...ok, size: 0 }));
  assert.throws(() => buildManifest({ ...ok, baseUrl: 'http://exemple.test' }));
});

const TOML = '[package]\nname = "stremio-shell-ng"\nversion = "1.0.17"\nedition = "2018"\n\n[dependencies]\nversion = "9"\n';
const LOCK = (v) => `[[package]]\nname = "autre"\nversion = "9.9.9"\n\n[[package]]\nname = "stremio-shell-ng"\nversion = "${v}"\n`;
const ISS = (v) => `#define MyAppName "Laststream"\n#define MyAppVersion "${v}"\n`;

test('lecture des versions Cargo.toml et Laststream.iss', () => {
  assert.equal(readCargoVersion(TOML), '1.0.17');
  assert.equal(readIssVersion(ISS('1.0.17')), '1.0.17');
  assert.throws(() => readIssVersion('rien'));
});

test('checkVersions signale chaque ecart', () => {
  assert.deepEqual(checkVersions({ cargoToml: TOML, cargoLock: LOCK('1.0.17'), iss: ISS('1.0.17') }), []);
  const iss = checkVersions({ cargoToml: TOML, cargoLock: LOCK('1.0.17'), iss: ISS('1.0.16') });
  assert.equal(iss.length, 1);
  assert.match(iss[0], /Laststream\.iss \(1\.0\.16\)/);
  const lock = checkVersions({ cargoToml: TOML, cargoLock: LOCK('1.0.16'), iss: ISS('1.0.17') });
  assert.equal(lock.length, 1);
  assert.match(lock[0], /Cargo\.lock/);
});

test('checkVersions echoue si le paquet est absent de Cargo.lock', () => {
  const problems = checkVersions({ cargoToml: TOML, cargoLock: '[[package]]\nname = "autre"\nversion = "9.9.9"\n', iss: ISS('1.0.17') });
  assert.equal(problems.length, 1);
  assert.match(problems[0], /introuvable dans Cargo\.lock/);
});

test('arg ignore une valeur absente ou qui est un autre drapeau (PowerShell 5.1 supprime les arguments vides)', () => {
  assert.equal(arg(['x.exe', '--notes', 'ok', '--out', 'f'], '--notes'), 'ok');
  assert.equal(arg(['x.exe', '--notes', '--out', 'f'], '--notes'), undefined);
  assert.equal(arg(['x.exe', '--notes', '--out', 'f'], '--out'), 'f');
  assert.equal(arg(['x.exe', '--notes'], '--notes'), undefined);
  assert.equal(arg(['x.exe'], '--notes'), undefined);
});
