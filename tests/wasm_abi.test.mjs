// ABI test: drive the REAL packdiff_wasm.wasm exactly the way the page's JS
// does (empty import object, (ptr,len) UTF-8 buffers, packed-u64 returns).
// Skips with a hint if the wasm artifact has not been built yet.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const here = path.dirname(fileURLToPath(import.meta.url));
const WASM_PATH = path.join(
  here, '..', 'target-wasm', 'wasm32-unknown-unknown', 'release', 'packdiff_wasm.wasm');

let ex = null;
try {
  const bytes = await readFile(WASM_PATH);
  ({ instance: { exports: ex } } = await WebAssembly.instantiate(bytes, {}));
} catch (e) {
  console.log(`SKIP: wasm module not found at ${WASM_PATH} — run cargo build --release first (${e.code ?? e})`);
}

const enc = new TextEncoder();
const dec = new TextDecoder();

function callRaw(name, ...strs) {
  const args = [];
  const allocs = [];
  for (const s of strs) {
    const b = enc.encode(s);
    const ptr = ex.pd_alloc(b.length);
    new Uint8Array(ex.memory.buffer, ptr, b.length).set(b);
    args.push(ptr, b.length);
    allocs.push([ptr, b.length]);
  }
  const packed = ex[name](...args);
  for (const [p, l] of allocs) ex.pd_free(p, l);
  const rptr = Number(packed >> 32n);
  const rlen = Number(packed & 0xffffffffn);
  const out = dec.decode(new Uint8Array(ex.memory.buffer, rptr, rlen).slice());
  ex.pd_free(rptr, rlen);
  return JSON.parse(out);
}
function call(name, ...strs) {
  const env = callRaw(name, ...strs);
  assert.ok('Ok' in env, `expected { "Ok": ... } from ${name}: ${JSON.stringify(env)}`);
  return env.Ok;
}

const META = JSON.stringify({
  repo: 'sample',
  base: { name: 'main', sha: 'a'.repeat(40) },
  head: { name: 'feature', sha: 'b'.repeat(40) },
});

function comment(id, line, text, updated) {
  return JSON.stringify({
    id, file: 'src/app.rs', side: 'New', line, text,
    created_at: '2026-07-03T10:00:00Z',
    updated_at: updated ?? '2026-07-03T10:00:00Z',
  });
}

test('exports exist', { skip: !ex }, () => {
  for (const name of ['pd_alloc', 'pd_free', 'pd_new_document', 'pd_parse_document',
    'pd_upsert_comment', 'pd_delete_comment', 'pd_merge',
    'pd_export_json', 'pd_export_markdown', 'pd_export_csv', 'pd_storage_key',
    'pd_markdown_html', 'pd_range_diff', 'pd_context_slice']) {
    assert.equal(typeof ex[name], 'function', name);
  }
});

test('new document + storage key', { skip: !ex }, () => {
  const doc = call('pd_new_document', META);
  assert.equal(doc.schema_version, 1);
  assert.equal(doc.tool, 'packdiff');
  assert.deepEqual(doc.comments, []);
  const key = call('pd_storage_key', META);
  assert.equal(key, `packdiff:v1:sample:${'a'.repeat(12)}..${'b'.repeat(12)}`);
});

test('upsert, edit, delete round-trip', { skip: !ex }, () => {
  let doc = call('pd_new_document', META);
  doc = call('pd_upsert_comment', JSON.stringify(doc), comment('c1', 42, 'first'));
  assert.equal(doc.comments.length, 1);
  doc = call('pd_upsert_comment', JSON.stringify(doc),
    comment('c1', 42, 'edited', '2026-07-03T11:00:00Z'));
  assert.equal(doc.comments.length, 1);
  assert.equal(doc.comments[0].text, 'edited');
  doc = call('pd_delete_comment', JSON.stringify(doc), 'c1');
  assert.equal(doc.comments.length, 0);
});

test('ordering is deterministic (file, line, side)', { skip: !ex }, () => {
  let doc = call('pd_new_document', META);
  doc = call('pd_upsert_comment', JSON.stringify(doc), comment('z', 9, 'later line'));
  doc = call('pd_upsert_comment', JSON.stringify(doc), comment('a', 2, 'early line'));
  assert.deepEqual(doc.comments.map((c) => c.id), ['a', 'z']);
});

test('merge unions by id, newer updated_at wins', { skip: !ex }, () => {
  let ours = call('pd_new_document', META);
  ours = call('pd_upsert_comment', JSON.stringify(ours), comment('shared', 1, 'mine'));
  let theirs = call('pd_new_document', META);
  theirs = call('pd_upsert_comment', JSON.stringify(theirs),
    comment('shared', 1, 'theirs-newer', '2026-07-03T12:00:00Z'));
  theirs = call('pd_upsert_comment', JSON.stringify(theirs), comment('extra', 3, 'new one'));
  const merged = call('pd_merge', JSON.stringify(ours), JSON.stringify(theirs));
  assert.equal(merged.comments.length, 2);
  assert.equal(merged.comments.find((c) => c.id === 'shared').text, 'theirs-newer');
});

test('exports: markdown groups by file, csv quotes, json reimports', { skip: !ex }, () => {
  let doc = call('pd_new_document', META);
  doc = call('pd_upsert_comment', JSON.stringify(doc),
    comment('c1', 42, 'multi\nline "note"'));
  const md = call('pd_export_markdown', JSON.stringify(doc));
  assert.match(md, /^# Review comments — sample main\.\.feature\n/);
  assert.ok(md.includes('## src/app.rs'));
  assert.ok(md.includes('- **L42 (new)** — multi\n  line "note"'));
  const csv = call('pd_export_csv', JSON.stringify(doc));
  assert.ok(csv.startsWith('"file","side","line","created_at","updated_at","text"\r\n'));
  assert.ok(csv.includes('"multi\nline ""note"""'));
  const json = call('pd_export_json', JSON.stringify(doc));
  const back = call('pd_parse_document', json);
  assert.deepEqual(back, doc);
});

test('markdown renders and hostile input stays escaped', { skip: !ex }, () => {
  assert.equal(call('pd_markdown_html', '**hi** `code`'),
    '<p><strong>hi</strong> <code>code</code></p>');
  assert.equal(call('pd_markdown_html', '<script>alert(1)</script>'),
    '<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>');
  const link = call('pd_markdown_html', '[x](javascript:alert(1))');
  assert.ok(!link.includes('<a '), link);
});

test('range diff over snapshots, exactly as the page calls it', { skip: !ex }, () => {
  const SNAPSHOTS = JSON.stringify({
    blobs: { one: 'alpha\nbeta\n', two: 'alpha\nBETA\n', bin: null },
    boundaries: [
      { sha: 's0', files: { 'a.txt': 'one' } },
      { sha: 's1', files: { 'a.txt': 'two', 'blob.bin': 'bin' } },
      { sha: 's2', files: { 'a.txt': 'one', 'blob.bin': 'bin' } },
    ],
  });
  const files = call('pd_range_diff', SNAPSHOTS,
    JSON.stringify({ from: 0, to: 1, context: 3 }));
  assert.equal(files.length, 2);
  const a = files.find((f) => f.new_path === 'a.txt');
  assert.equal(a.status, 'Modified');
  assert.deepEqual(a.hunks[0].lines[1], { Del: { old: 2, text: 'beta' } });
  assert.deepEqual(a.hunks[0].lines[2], { Add: { new: 2, text: 'BETA' } });
  assert.ok(files.find((f) => f.new_path === 'blob.bin').binary);
  // The full range hides the reverted a.txt; blob.bin is unchanged after s1.
  const full = call('pd_range_diff', SNAPSHOTS,
    JSON.stringify({ from: 0, to: 2, context: 3 }));
  assert.deepEqual(full.map((f) => f.new_path), ['blob.bin']);
  // Errors come back as envelopes, not traps.
  const bad = callRaw('pd_range_diff', SNAPSHOTS,
    JSON.stringify({ from: 2, to: 1, context: 3 }));
  assert.ok('Error' in bad);
  assert.match(bad.Error.message, /invalid snapshot range/);
});

test('context slice between the endpoint snapshots', { skip: !ex }, () => {
  const SNAPSHOTS = JSON.stringify({
    blobs: { one: 'alpha\nbeta\ngamma\n', two: 'alpha\nBETA\ngamma\n' },
    boundaries: [
      { sha: 's0', files: { 'a.txt': 'one' } },
      { sha: 's1', files: { 'a.txt': 'two' } },
      { sha: 's2', files: { 'a.txt': 'one' } },
    ],
  });
  // a.txt is identical at the endpoints (the middle boundary does not matter).
  const lines = call('pd_context_slice', SNAPSHOTS, JSON.stringify(
    { old_path: 'a.txt', new_path: 'a.txt', old_start: 1, new_start: 1, count: 10 }));
  assert.equal(lines.length, 3, 'clamped at end of file');
  assert.deepEqual(lines[0], { Ctx: { old: 1, new: 1, text: 'alpha' } });
  assert.deepEqual(lines[2], { Ctx: { old: 3, new: 3, text: 'gamma' } });
  // Unknown paths come back as envelopes, not traps.
  const bad = callRaw('pd_context_slice', SNAPSHOTS, JSON.stringify(
    { old_path: 'nope', new_path: 'nope', old_start: 1, new_start: 1, count: 1 }));
  assert.ok('Error' in bad);
  assert.match(bad.Error.message, /does not exist/);
});

test('error envelopes: invalid comment, garbage, newer schema, unknown field', { skip: !ex }, () => {
  const doc = call('pd_new_document', META);
  const bad = callRaw('pd_upsert_comment', JSON.stringify(doc),
    JSON.stringify({ id: 'x', file: 'f', side: 'New', line: 0, text: 'hi',
      created_at: 't', updated_at: 't' }));
  assert.ok('Error' in bad, JSON.stringify(bad));
  assert.match(bad.Error.message, /line must be >= 1/);

  const garbage = callRaw('pd_parse_document', 'not json at all');
  assert.ok('Error' in garbage);

  const future = { ...doc, schema_version: 99 };
  const rejected = callRaw('pd_parse_document', JSON.stringify(future));
  assert.ok('Error' in rejected);
  assert.match(rejected.Error.message, /unsupported schema_version 99/);

  const sneaky = { ...doc, sneaky: true };
  const denied = callRaw('pd_parse_document', JSON.stringify(sneaky));
  assert.ok('Error' in denied, 'unknown fields are strict-rejected');
});
