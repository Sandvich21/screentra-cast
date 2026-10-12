#!/usr/bin/env node
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {spawnSync} = require('node:child_process');

const root = path.resolve(__dirname, '..');
const staticDir = path.join(root, 'static');
const htmlFiles = fs.readdirSync(staticDir)
  .filter((name) => name.endsWith('.html'))
  .map((name) => path.join(staticDir, name));

for (const file of htmlFiles) {
  const html = fs.readFileSync(file, 'utf8');
  assert.doesNotMatch(html, /<script\s*>/i, `${path.basename(file)} still contains an inline script element`);
  for (const src of html.matchAll(/<script\s+[^>]*src=["']([^"']+)["'][^>]*><\/script>/gi)) {
    const urlPath = src[1].split('?', 1)[0];
    if (!urlPath.startsWith('/js/')) continue;
    const jsFile = path.join(root, 'static', urlPath.slice(1));
    assert(fs.existsSync(jsFile), `${path.basename(file)} references missing ${urlPath}`);
    const check = spawnSync('node', ['--check', jsFile], {encoding: 'utf8'});
    assert.equal(check.status, 0, check.stderr || `${jsFile} failed syntax validation`);
  }
}

console.log('CSP inline-script integration: static HTML uses external same-origin scripts');
