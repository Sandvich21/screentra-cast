#!/usr/bin/env node
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {spawnSync} = require('node:child_process');

const root = path.resolve(__dirname, '..');
const installerPath = path.join(root, 'static/install/player-client.sh');
const installer = fs.readFileSync(installerPath, 'utf8');

function extract(marker, endMarker) {
  const start = installer.indexOf(marker);
  assert.notEqual(start, -1, `missing ${marker}`);
  const bodyStart = start + marker.length;
  const end = installer.indexOf(endMarker, bodyStart);
  assert.notEqual(end, -1, `missing ${endMarker}`);
  return installer.slice(bodyStart, end).replace(/^\n/, '');
}

const shell = spawnSync('bash', ['-n', installerPath], {encoding: 'utf8'});
assert.equal(shell.status, 0, shell.stderr || 'installer shell syntax failed');

const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'screentra-cast-update-test-'));
try {
  for (const [marker, endMarker, filename] of [
    ["cat > \"$ACTOR\" <<'PY_ACTOR'", 'PY_ACTOR', 'actor.py'],
    ["run_privileged tee \"$UPDATER_SCRIPT\" >/dev/null <<'PY_UPDATER'", 'PY_UPDATER', 'updater.py'],
  ]) {
    const file = path.join(tempDir, filename);
    fs.writeFileSync(file, extract(marker, endMarker));
    const result = spawnSync('python3', ['-m', 'py_compile', file], {encoding: 'utf8'});
    assert.equal(result.status, 0, result.stderr || `${filename} syntax failed`);
  }

  const updateStart = installer.indexOf('def update_client(');
  const executeStart = installer.indexOf('\ndef execute(', updateStart);
  assert(updateStart >= 0 && executeStart > updateStart, 'actor update handler missing');
  const actorUpdate = installer.slice(updateStart, executeStart);
  assert.match(actorUpdate, /\/var\/lib\/screentra-cast\/update\/request/);
  assert.match(actorUpdate, /verified root client updater requested/);
  assert.doesNotMatch(actorUpdate, /urllib\.request\.urlopen/);
  assert.doesNotMatch(actorUpdate, /subprocess\.run/);

  assert.match(installer, /screentra-client-updater\.service/);
  assert.match(installer, /screentra-client-updater\.path/);
  assert.match(installer, /PathExists=\$UPDATE_REQUEST_FILE/);
  const updater = extract("run_privileged tee \"$UPDATER_SCRIPT\" >/dev/null <<'PY_UPDATER'", 'PY_UPDATER');
  assert.match(updater, /X-SCREENTRA-CLIENT-TOKEN/);
  assert.match(updater, /ssl\.create_default_context\(cafile=str\(ca_file\)\)/);
  assert.match(updater, /openssl\", \"pkeyutl\", \"-verify/);
  assert.match(updater, /expected_path: str/);
  assert.match(updater, /same_origin\(server, str\(manifest\.get\("artifact_url"\)/);
  assert.match(updater, /DIGEST_FILE/);

  const api = fs.readFileSync(path.join(root, 'src/api.rs'), 'utf8');
  const middleware = fs.readFileSync(path.join(root, 'src/middleware.rs'), 'utf8');
  assert.match(api, /\/api\/clients\/\{id\}\/update-check/);
  assert.match(api, /async fn get_client_update_check/);
  assert.match(api, /require_client_command_token\(&repository, &id, &headers\)/);
  assert.match(middleware, /ends_with\("\/update-check"\)/);
} finally {
  fs.rmSync(tempDir, {recursive: true, force: true});
}

console.log('client update service integration: board request is root-verified and signed');
