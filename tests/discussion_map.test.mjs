// The packdiff-pr skill's map-discussion.mjs transform: a GitHub PR
// discussion bundle in, review.json + discussion.md out. The interesting
// assertion is the last one — the generated review.json is fed through the
// REAL wasm module's pd_parse_document and pd_merge, proving the file the
// skill ships is importable by the page, not merely shaped like it.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const here = path.dirname(fileURLToPath(import.meta.url));
const SCRIPT = path.join(here, '..', '.skills', 'packdiff-pr', 'map-discussion.mjs');

const BUNDLE = {
  pr: {
    number: 7,
    title: 'Teach the widget to frobnicate',
    url: 'https://github.com/acme/widget/pull/7',
    state: 'OPEN',
    reviewDecision: 'APPROVED',
    repo: 'widget',
    base: { name: 'main', sha: 'a'.repeat(40) },
    head: { name: 'frob', sha: 'b'.repeat(40) },
  },
  threads: [
    {
      isResolved: false, isOutdated: false,
      path: 'src/app.rs', line: 42, originalLine: 42, diffSide: 'RIGHT',
      comments: { nodes: [
        { databaseId: 101, url: 'https://github.com/acme/widget/pull/7#discussion_r101',
          body: 'Should this handle zero?', createdAt: '2026-07-20T10:00:00Z',
          lastEditedAt: null, author: { login: 'reviewer-one' } },
        { databaseId: 102, url: 'https://github.com/acme/widget/pull/7#discussion_r102',
          body: 'Good catch — fixed in the next push.\r\nWith a test.',
          createdAt: '2026-07-20T11:00:00Z',
          lastEditedAt: '2026-07-20T11:05:00Z', author: { login: 'author-dev' } },
      ] },
    },
    {
      isResolved: true, isOutdated: true,
      path: 'src/lib.rs', line: null, originalLine: 9, diffSide: 'LEFT',
      comments: { nodes: [
        { databaseId: 103, url: 'https://github.com/acme/widget/pull/7#discussion_r103',
          body: 'This branch is dead now.', createdAt: '2026-07-19T09:00:00Z',
          lastEditedAt: null, author: null },
      ] },
    },
  ],
  reviews: [
    { id: 1, user: { login: 'reviewer-one' }, state: 'CHANGES_REQUESTED',
      submitted_at: '2026-07-19T09:30:00Z', body: 'A few things first.' },
    { id: 2, user: { login: 'reviewer-one' }, state: 'APPROVED',
      submitted_at: '2026-07-21T08:00:00Z', body: '' },
  ],
  conversation: [
    { id: 3, user: { login: 'driveby' }, body: 'Excited for this!',
      created_at: '2026-07-18T12:00:00Z', updated_at: '2026-07-18T12:00:00Z' },
  ],
};

function runScript(bundle) {
  const dir = mkdtempSync(path.join(tmpdir(), 'packdiff-discussion-'));
  try {
    const bundlePath = path.join(dir, 'bundle.json');
    const jsonPath = path.join(dir, 'review.json');
    const mdPath = path.join(dir, 'discussion.md');
    writeFileSync(bundlePath, JSON.stringify(bundle));
    const summary = JSON.parse(execFileSync(
      process.execPath, [SCRIPT, bundlePath, jsonPath, mdPath], { encoding: 'utf8' }));
    return {
      summary,
      doc: JSON.parse(readFileSync(jsonPath, 'utf8')),
      md: readFileSync(mdPath, 'utf8'),
    };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test('review.json carries anchored threads, resolution, and the verdict', () => {
  const { summary, doc } = runScript(BUNDLE);
  assert.deepEqual(summary.Mapped, {
    threads: 2, comments: 3, fallback_anchored: 1,
    reviews: 2, conversation: 1, verdict: 'Approved',
  });

  assert.equal(doc.schema_version, 3);
  assert.equal(doc.tool, 'packdiff');
  assert.equal(doc.repo, 'widget');
  assert.equal(doc.comments.length, 3);

  const [q, a] = doc.comments.filter((c) => c.file === 'src/app.rs');
  assert.deepEqual(
    [q.id, q.side, q.line, q.resolved_at], ['github-rc-101', 'New', 42, undefined]);
  assert.match(q.text, /^\*\*@reviewer-one\*\* · 2026-07-20T10:00:00Z/);
  assert.match(q.text, /Should this handle zero\?/);
  assert.equal(a.updated_at, '2026-07-20T11:05:00Z');
  assert.ok(a.text.includes('fixed in the next push.\nWith a test.'), 'CRLF normalized');
  assert.deepEqual(a.version, { seq: 1, actor: 'github:author-dev' });

  // The outdated resolved thread: originalLine fallback, Old side, ghost
  // author, resolved_at stamped from the thread's last activity.
  const dead = doc.comments.find((c) => c.file === 'src/lib.rs');
  assert.deepEqual(
    [dead.id, dead.side, dead.line, dead.resolved_at],
    ['github-rc-103', 'Old', 9, '2026-07-19T09:00:00Z']);
  assert.match(dead.text, /@ghost/);
  assert.match(dead.text, /outdated thread/);

  // reviewDecision APPROVED, stamped at the newest APPROVED review.
  assert.deepEqual(doc.verdict, { Approved: { at: '2026-07-21T08:00:00Z' } });
  assert.deepEqual(doc.verdict_version, { seq: 1, actor: 'github:reviewer-one' });
});

test('discussion.md carries the whole discussion, grouped and labeled', () => {
  const { md } = runScript(BUNDLE);
  assert.match(md, /^# PR discussion — widget#7: Teach the widget to frobnicate/);
  assert.match(md, /2 review thread\(s\) \(1 resolved, 1 outdated\)/);
  assert.match(md, /## Reviews\n/);
  assert.match(md, /\*\*@reviewer-one\*\* — APPROVED · 2026-07-21T08:00:00Z/);
  assert.match(md, /## Conversation\n/);
  assert.match(md, /Excited for this!/);
  assert.match(md, /### src\/app\.rs\n/);
  assert.match(md, /\*\*L42 \(new\)\*\*\n/);
  assert.match(md, /\*\*L9 \(old\)\*\* — resolved, outdated/);
});

test('no decision, no threads: verdict absent, empty document still valid', () => {
  const { summary, doc, md } = runScript({
    pr: { ...BUNDLE.pr, reviewDecision: 'REVIEW_REQUIRED' },
    threads: [], reviews: [], conversation: [],
  });
  assert.equal(summary.Mapped.verdict, null);
  assert.equal(doc.verdict, undefined);
  assert.deepEqual(doc.comments, []);
  assert.match(md, /0 review thread\(s\)/);
});

// ---- the proof: the page's real engine accepts what the skill ships ----

const WASM_PATH = path.join(
  here, '..', 'target-wasm', 'wasm32-unknown-unknown', 'release', 'packdiff_wasm.wasm');
let ex = null;
try {
  const bytes = await readFile(WASM_PATH);
  ({ instance: { exports: ex } } = await WebAssembly.instantiate(bytes, {}));
} catch (e) {
  console.log(`SKIP: wasm module not found at ${WASM_PATH} — run cargo build --release first (${e.code ?? e})`);
}

function call(name, ...strs) {
  const enc = new TextEncoder();
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
  const out = new TextDecoder().decode(
    new Uint8Array(ex.memory.buffer, rptr, rlen).slice());
  ex.pd_free(rptr, rlen);
  const env = JSON.parse(out);
  assert.ok('Ok' in env, `expected { "Ok": ... } from ${name}: ${out}`);
  return env.Ok;
}

test('the real wasm engine parses and merges the generated review.json', { skip: !ex }, () => {
  const { doc } = runScript(BUNDLE);
  const raw = JSON.stringify(doc);

  const parsed = call('pd_parse_document', raw);
  assert.equal(parsed.comments.length, 3);
  assert.deepEqual(parsed.verdict, { Approved: { at: '2026-07-21T08:00:00Z' } });

  // Import into a fresh local store, exactly as the page's Import JSON does.
  const local = call('pd_new_document', JSON.stringify({
    repo: 'local-name', base: doc.base, head: doc.head }));
  const merged = call('pd_merge', JSON.stringify(local), raw);
  assert.equal(merged.comments.length, 3);
  assert.equal(merged.repo, 'local-name', 'importing never re-targets the store');
  assert.deepEqual(merged.verdict, { Approved: { at: '2026-07-21T08:00:00Z' } });

  // Idempotent: importing the same file twice changes nothing.
  const again = call('pd_merge', JSON.stringify(merged), raw);
  assert.deepEqual(again, merged);
});
