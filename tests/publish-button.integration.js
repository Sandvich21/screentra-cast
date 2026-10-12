const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const editorSource = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'editor.js'), 'utf8');
const indexSource = fs.readFileSync(path.join(__dirname, '..', 'static', 'index.html'), 'utf8');

assert.match(indexSource, /id="btn-editor-publish"[^>]*>Publish<\/button>/);
assert.match(editorSource, /E\.publishBtn\.onclick = \(\) => this\.publishCurrent\(\)/);

const calls = [];
const context = {
  console,
  API: {
    async publishLayout(layoutId) {
      calls.push(['publishLayout', layoutId]);
      return { refresh_epoch: 42 };
    },
    async getLayoutPropagationStatus(layoutId) {
      calls.push(['getLayoutPropagationStatus', layoutId]);
      return {
        target_count: 1,
        acknowledged_count: 1,
        pending_count: 0,
        offline_count: 0,
      };
    },
  },
};
vm.runInNewContext(`${editorSource}\n;globalThis.Editor = Editor;`, context, { filename: 'editor.js' });

const editor = context.Editor;
const status = { textContent: '' };
const busyStates = [];
let saveCount = 0;
editor.layouts = [{ id: 'layout-1', name: 'Test layout', widgets: [] }];
editor.currentLayoutId = 'layout-1';
editor.el = { publishStatus: status };
editor.saveCurrent = async () => { saveCount += 1; };
editor.updateInfo = () => { busyStates.push(editor._publishBusy); };

(async () => {
  await editor.publishCurrent();

  assert.strictEqual(saveCount, 1, 'Publish must save the current layout first');
  assert.deepStrictEqual(calls, [
    ['publishLayout', 'layout-1'],
    ['getLayoutPropagationStatus', 'layout-1'],
  ]);
  assert.strictEqual(status.textContent, 'Published · 1/1 displays acknowledged');
  assert.deepStrictEqual(busyStates, [true, false]);
  assert.strictEqual(editor._publishBusy, false);

  console.log('publish button integration: save, publish, propagation status passed');
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
