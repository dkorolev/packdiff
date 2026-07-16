// Browser interaction tests against a real generated packdiff HTML page.
// Opens the artifact over file:// so offline/self-contained behavior is
// exercised. Requires: git, a built packdiff binary, and Playwright browsers.
import { test, expect } from '@playwright/test';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, writeFileSync, rmSync, existsSync, mkdirSync, statSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');

function git(repo, args) {
  const r = spawnSync('git', ['-C', repo, '-c', 'user.name=Test', '-c', 'user.email=test@example.com', ...args], {
    encoding: 'utf8',
  });
  if (r.status !== 0) throw new Error(`git ${args.join(' ')} failed: ${r.stderr}`);
}

function write(repo, rel, content) {
  const p = join(repo, rel);
  mkdirSync(dirname(p), { recursive: true });
  writeFileSync(p, content);
}

function findBinary() {
  const candidates = [
    process.env.PACKDIFF_BIN,
    join(root, 'target', 'debug', 'packdiff'),
    join(root, 'target', 'release', 'packdiff'),
  ].filter(Boolean);
  for (const c of candidates) {
    if (existsSync(c)) return c;
  }
  // Build debug binary if missing
  const build = spawnSync('cargo', ['build', '-p', 'packdiff', '--bin', 'packdiff'], {
    cwd: root,
    encoding: 'utf8',
  });
  if (build.status !== 0) throw new Error(build.stderr || build.stdout);
  return join(root, 'target', 'debug', 'packdiff');
}

function makeFixture() {
  const dir = mkdtempSync(join(tmpdir(), 'packdiff-browser-'));
  const repo = join(dir, 'sample');
  mkdirSync(repo);
  git(repo, ['init', '-q']);
  git(repo, ['symbolic-ref', 'HEAD', 'refs/heads/main']);
  write(repo, 'hello.py', "def hello():\n    return 'hello'\n");
  write(repo, 'todelete.txt', 'obsolete\n');
  write(repo, 'notes.md', '# Title\n\nBody paragraph.\n');
  write(repo, 'blob.bin', Buffer.from([0, 1, 2, 66, 73, 78]));
  // 60 lines with a mid-file change coming: hunk gaps on both sides, for the
  // expand-context tests.
  const calcLine = (i) => `line ${i + 1}`;
  write(repo, 'calc.py', Array.from({ length: 60 }, (_, i) => calcLine(i)).join('\n') + '\n');
  const thresholdLine = (i) => `threshold line ${i + 1}`;
  write(repo, 'context-threshold.txt',
    Array.from({ length: 30 }, (_, i) => thresholdLine(i)).join('\n') + '\n');
  git(repo, ['add', '-A']);
  git(repo, ['commit', '-qm', 'initial']);

  git(repo, ['checkout', '-qb', 'feature']);
  write(repo, 'hello.py', "def hello():\n    return 'hello'\n\ndef evil():\n    return 42\n");
  write(repo, 'calc.py',
    Array.from({ length: 60 }, (_, i) => (i === 49 ? 'line 50 CHANGED' : calcLine(i))).join('\n') + '\n');
  write(repo, 'context-threshold.txt', Array.from({ length: 30 }, (_, i) =>
    ([4, 13, 23].includes(i) ? `threshold line ${i + 1} CHANGED` : thresholdLine(i))).join('\n') + '\n');
  git(repo, ['rm', '-q', 'todelete.txt']);
  write(repo, 'notes.md', '# Title\n\nBody paragraph updated.\n\n## More\n');
  write(repo, 'newfile.md', '# New\n\nBrand new file.\n');
  write(repo, 'blob.bin', Buffer.from([0, 1, 2, 67, 72, 71]));
  git(repo, ['add', '-A']);
  git(repo, ['commit', '-qm', 'feature one']);
  write(repo, 'extra.txt', 'second commit\n');
  git(repo, ['add', '-A']);
  git(repo, ['commit', '-qm', 'feature two']);

  // Notes commit: lifts into the Description panel (Markdown | Source).
  write(repo, 'PR-DESCRIPTION.md',
    '# Fixture PR\n\nA short description body.\n\n- Parent item\n  - Child item\n    - Grandchild item\n- Sibling item\n');
  git(repo, ['add', '-A']);
  const notes = spawnSync(
    'git',
    ['-C', repo, '-c', 'user.name=NotesBot', '-c', 'user.email=notes-bot@example.com',
      'commit', '-qm', 'pr notes'],
    { encoding: 'utf8' },
  );
  if (notes.status !== 0) throw new Error(`notes commit failed: ${notes.stderr}`);

  const out = join(dir, 'review.html');
  const bin = findBinary();
  const r = spawnSync(bin, ['main', 'feature', '-C', repo, '-o', out], {
    encoding: 'utf8',
    env: { ...process.env, PACKDIFF_SYSTEM_USER_EMAIL: 'notes-bot@example.com' },
  });
  if (r.status !== 0) throw new Error(`packdiff failed: ${r.stderr || r.stdout}`);
  return { dir, out, fileUrl: pathToFileURL(out).href };
}

let fixture;

test.beforeAll(() => {
  fixture = makeFixture();
});

test.afterAll(() => {
  if (fixture?.dir) rmSync(fixture.dir, { recursive: true, force: true });
});

test.beforeEach(async ({ page, context }) => {
  page.on('pageerror', (error) => console.error('pageerror:', error.stack));
  // Isolate localStorage between tests
  await context.clearCookies();
  await page.goto(fixture.fileUrl);
  await page.evaluate(() => localStorage.clear());
  await page.goto(fixture.fileUrl);
  // Wait for WASM + comment engine
  await page.waitForFunction(() => {
    const el = document.getElementById('comment-count');
    return el && /comment/.test(el.textContent || '');
  });
});

test('page is self-contained: no network requests after load', async ({ page }) => {
  const requests = [];
  page.on('request', (req) => requests.push(req.url()));
  await page.goto(fixture.fileUrl);
  await page.waitForTimeout(500);
  // file:// document itself may appear; reject http(s)
  const external = requests.filter((u) => /^https?:/i.test(u));
  expect(external, `unexpected network: ${external.join(', ')}`).toEqual([]);
});

test('document summary and stable navigation are present without a sidebar', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  const nav = page.locator('#topnav.app-chrome');
  await expect(nav).toBeVisible();
  expect(await nav.evaluate((el) => el.getBoundingClientRect().top)).toBe(0);
  await expect(page.locator('#review-back')).toHaveText('← Back');
  await expect(nav.locator('.section-link').first()).toHaveText('Summary');
  await expect(page.locator('#summary .review-summary-text')).toBeVisible();
  expect(await page.locator('#summary').evaluate((el) => el.getBoundingClientRect().top))
    .toBeGreaterThanOrEqual(await nav.evaluate((el) => el.getBoundingClientRect().bottom));
  await expect(page.locator('#sidebar')).toHaveCount(0);
  const firstFile = page.locator('#files-full details.file').first();
  await expect(firstFile).toBeAttached();
});

test('navigation stays pinned and highlights the section being scrolled', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 500 });
  const nav = page.locator('#topnav');
  const sectionIds = ['summary', 'description', 'commits', 'files', 'diff'];
  for (const id of sectionIds) {
    await page.locator('#' + id).evaluate((el) => el.scrollIntoView({ block: 'start' }));
    await expect(nav.locator('.section-link[href="#' + id + '"]')).toHaveAttribute('aria-current', 'location');
    const top = await nav.evaluate((el) => el.getBoundingClientRect().top);
    expect(top).toBe(0);
  }
  await expect(nav.locator('.section-link.active')).toHaveCount(1);
});

test('Back control returns to the page that opened the review', async ({ page }) => {
  await page.goto('about:blank');
  await page.goto(fixture.fileUrl);
  const historyLength = await page.evaluate(() => history.length);
  for (const id of ['summary', 'description', 'commits', 'files', 'diff']) {
    await page.locator(`.section-link[href="#${id}"]`).click();
  }
  expect(await page.evaluate(() => history.length)).toBe(historyLength);
  await page.locator('#review-back').click();
  await expect(page).toHaveURL('about:blank');
});

test('comment gutter creates, edits, and deletes a comment', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  // Prefer a visible unified-diff gutter (skip markdown files that start in Preview).
  await page.evaluate(() => {
    for (const d of document.querySelectorAll('#files-full details.file')) {
      d.open = true;
      const wrap = d.querySelector('.diff-wrap');
      const preview = d.querySelector('.md-preview');
      if (wrap && preview) {
        preview.hidden = true;
        wrap.hidden = false;
      }
    }
  });
  // Prefer a mid-file line so sticky chrome/file headers do not cover the control.
  const gutter = page.locator('#files-full .diff-wrap:not([hidden]) tr.commentable .gutter-btn').nth(1);
  await gutter.evaluate((el) => {
    el.scrollIntoView({ block: 'center', inline: 'nearest' });
    el.click();
  });
  const editor = page.locator('.pd-editor textarea');
  await expect(editor).toBeVisible({ timeout: 10_000 });
  await editor.fill('**hello** from test');
  await page.locator('.pd-editor button.primary').click();
  await expect(page.locator('.comment-card').first()).toBeVisible();
  await expect(page.locator('#comment-count')).toContainText('1 comment');
  // The global count navigates directly to inline review data; no drawer.
  await page.locator('#comment-count').click();
  await expect(page.locator('.comment-card').first()).toBeFocused();
  // Deletion is an inline two-step mutation and remains undoable.
  const del = page.locator('.comment-card .comment-actions button', { hasText: 'Delete' });
  await del.click();
  await expect(del).toContainText('Confirm delete');
  await del.click();
  await expect(page.locator('.comment-card')).toHaveCount(0);
  await page.locator('#undo-change').click();
  await expect(page.locator('.comment-card').first()).toBeVisible();
});

test('older-generation stores are absorbed by merge, never clobbered', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  const ready = () => page.waitForFunction(() => {
    const el = document.getElementById('comment-count');
    return el && /comment/.test(el.textContent || '');
  });
  const plantV1 = (comments) => page.evaluate((comments) => {
    const cfg = JSON.parse(document.getElementById('packdiff-config').textContent);
    localStorage.setItem('packdiff:v1:diff:' + cfg.review_id, JSON.stringify({
      schema_version: 1, tool: 'packdiff', repo: cfg.repo, base: cfg.base, head: cfg.head,
      comments: comments,
    }));
  }, comments);
  // A v1 page wrote a comment under its own generation's key: this page
  // absorbs it on load.
  await plantV1([{ id: 'old1', file: 'hello.py', side: 'New', line: 2, text: 'from an old page',
    created_at: '2026-07-10T00:00:00Z', updated_at: '2026-07-10T00:00:00Z' }]);
  await page.reload();
  await ready();
  await expect(page.locator('#comment-count')).toContainText('1 comment');
  // Delete it here: the UNCHANGED old store must not resurrect it on reload.
  const del = page.locator('.comment-card .comment-actions button', { hasText: 'Delete' });
  await del.click();
  await expect(del).toContainText('Confirm delete');
  await del.click();
  await expect(page.locator('.comment-card')).toHaveCount(0);
  await page.reload();
  await ready();
  await expect(page.locator('.comment-card')).toHaveCount(0);
  // A CHANGED old store (someone kept writing in the old page) is absorbed
  // again — old-page work flows forward instead of being lost.
  await plantV1([{ id: 'old2', file: 'hello.py', side: 'New', line: 2, text: 'newer old-page note',
    created_at: '2026-07-11T00:00:00Z', updated_at: '2026-07-11T00:00:00Z' }]);
  await page.reload();
  await ready();
  await expect(page.locator('#comment-count')).toContainText('1 comment');
  await expect(page.locator('.comment-card')).toContainText('newer old-page note');
});

test('review outcome contributes at most one final action', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  // Comment is the default no-verdict outcome and is not an action.
  await expect(page.locator('#verdict-comment')).toHaveAttribute('aria-pressed', 'true');
  await expect(page.locator('#change-count')).toBeHidden();

  // A submitted verdict contributes exactly one final action.
  await page.locator('#verdict-changes').click();
  await expect(page.locator('#verdict-changes')).toHaveAttribute('aria-pressed', 'true');
  await expect(page.locator('#change-count')).toHaveText('1 change');
  await expect(page.locator('#undo-change')).toBeEnabled();
  await page.reload();
  await page.waitForFunction(() => {
    const el = document.getElementById('comment-count');
    return el && /comment/.test(el.textContent || '');
  });
  await expect(page.locator('#verdict-changes')).toHaveAttribute('aria-pressed', 'true');
  await page.locator('#verdict-approve').click();
  await expect(page.locator('#verdict-approve')).toHaveAttribute('aria-pressed', 'true');
  await expect(page.locator('#verdict-changes')).toHaveAttribute('aria-pressed', 'false');
  await expect(page.locator('#change-count')).toHaveText('1 change');

  // Undo removes that one final verdict and returns to Comment.
  await page.locator('#undo-change').click();
  await expect(page.locator('#verdict-comment')).toHaveAttribute('aria-pressed', 'true');
  await expect(page.locator('#change-count')).toBeHidden();

  // Returning to Comment removes the verdict action.
  await page.locator('#verdict-approve').click();
  await page.locator('#verdict-comment').click();
  await expect(page.locator('#verdict-comment')).toHaveAttribute('aria-pressed', 'true');
  await expect(page.locator('#change-count')).toBeHidden();
  await expect(page.locator('#undo-change')).toBeDisabled();

  // Repeated verdict entries written by an older page compact to one action.
  await page.evaluate(() => {
    const docKey = Object.keys(localStorage).find((key) => /^packdiff:v\d+:diff:[^:]+$/.test(key));
    localStorage.setItem(docKey + ':activity', JSON.stringify([
      { kind: 'verdict', label: 'Changes required', at: '2026-07-14T09:00:00Z' },
      { kind: 'verdict', label: 'Change approved', at: '2026-07-14T09:01:00Z' },
    ]));
  });
  await page.reload();
  await page.waitForFunction(() => document.getElementById('verdict-approve'));
  await expect(page.locator('#change-count')).toHaveText('1 change');
  await expect(page.locator('#activity-body')).toContainText('Change approved');

  // Review outcomes are independent action buttons, not a segmented
  // presentation-mode control such as Unified / Side-by-side.
  await expect(page.locator('.verdict-seg')).toHaveCount(0);
  await page.locator('#verdict-approve').focus();
  await expect(page.locator('#verdict-approve')).toHaveCSS('outline-style', 'solid');

  // Resolve: a resolved comment dims in place and the chrome count splits.
  await page.evaluate(() => {
    for (const d of document.querySelectorAll('#files-full details.file')) {
      d.open = true;
      const wrap = d.querySelector('.diff-wrap');
      const preview = d.querySelector('.md-preview');
      if (wrap && preview) { preview.hidden = true; wrap.hidden = false; }
    }
  });
  const gutter = page.locator('#files-full .diff-wrap:not([hidden]) tr.commentable .gutter-btn').nth(1);
  await gutter.evaluate((el) => {
    el.scrollIntoView({ block: 'center', inline: 'nearest' });
    el.click();
  });
  const editor = page.locator('.pd-editor textarea');
  await editor.fill('needs a second look');
  await page.locator('.pd-editor button.primary').click();
  const card = page.locator('.comment-card').first();
  await expect(card).toBeVisible();
  await card.locator('.comment-actions button', { hasText: 'Resolve' }).click();
  await expect(page.locator('.comment-card').first()).toHaveClass(/resolved/);
  await expect(page.locator('#comment-count')).toContainText('1 comment · 0 open');
  await page.locator('.comment-card .comment-actions button', { hasText: 'Reopen' }).click();
  await expect(page.locator('#comment-count')).toHaveText('1 comment');
});

test('hunk context expands into commentable rows', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  const calc = page.locator('#files-full details.file[data-anchor="calc.py"]');
  await calc.evaluate((el) => { el.open = true; el.scrollIntoView({ block: 'center' }); });
  // calc.py changes line 50 of 60: a 46-line gap above the hunk, 7 below.
  const expanders = calc.locator('table.diff.unified tr.expander .expander-btn');
  await expect(expanders).toHaveCount(2);
  await expect(expanders.first()).toHaveText('Show 20 of 46 hidden lines');
  await expect(expanders.last()).toHaveText('Show all 7 hidden lines');
  const ctxRows = calc.locator('table.diff.unified tr.ctx');
  const before = await ctxRows.count();
  await expanders.first().click();
  await expect(ctxRows).toHaveCount(before + 20);
  await expect(ctxRows.filter({ has: page.locator('.tok-num') })).not.toHaveCount(0);
  await expect(expanders.first()).toHaveText('Show 20 of 26 hidden lines');
  // Revealed lines are real comment targets (bottom of the gap: 27..46).
  const gutter = calc.locator('tr.commentable[data-line="30"] .gutter-btn');
  await gutter.evaluate((el) => { el.scrollIntoView({ block: 'center' }); el.click(); });
  const editor = page.locator('.pd-editor textarea');
  await editor.fill('note on an expanded line');
  await page.locator('.pd-editor button.primary').click();
  await expect(calc.locator('.comment-card')).toHaveCount(1);
  // The small trailing gap empties in one click and the control retires.
  await expanders.last().click();
  await expect(calc.locator('table.diff.unified tr.expander')).toHaveCount(1);
});

test('short hunk gaps stay visible while three-line gaps collapse', async ({ page }) => {
  const file = page.locator('#files-full details.file[data-anchor="context-threshold.txt"]');
  await file.evaluate((el) => { el.open = true; });
  // With three context lines, this fixture leaves gaps of 1, 2, 3, and 3
  // unchanged lines around its hunks. Only the latter two get controls.
  const expanders = file.locator('table.diff.unified tr.expander .expander-btn');
  await expect(expanders).toHaveCount(2);
  await expect(expanders).toHaveText(['Show all 3 hidden lines', 'Show all 3 hidden lines']);
  for (const line of [1, 9, 10]) {
    await expect(file.locator(`table.diff.unified tr.ctx[data-line="${line}"]`)).toHaveCount(1);
  }
});

test('Markdown/Source pill switches without losing the panel', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  const md = page.locator('#files-full details.file').filter({ has: page.locator('.md-seg') }).first();
  await expect(md.locator('.md-seg button')).toHaveText(['Markdown', 'Source']);
  await expect(md.locator('.md-preview')).toBeVisible();
  await md.locator('.md-seg button[data-mdview="diff"]').click();
  await expect(md.locator('.diff-wrap')).toBeVisible();
  await md.locator('.md-seg button[data-mdview="preview"]').click();
  await expect(md.locator('.md-preview')).toBeVisible();
});

test('Description Markdown/Source toggles rendered and source views', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  const desc = page.locator('#description');
  await expect(desc).toBeVisible();
  await expect(desc.locator('.md-preview')).toBeVisible();
  await expect(desc.locator('.desc-seg button')).toHaveText(['Markdown', 'Source']);
  await expect(desc.locator('h1')).toContainText('Fixture PR');
  await expect(desc.locator('.md-preview .md-list-fragment')).toHaveCount(4);
  await expect(desc.locator('.md-block[data-line="5"]')).toContainText('Parent item');
  await expect(desc.locator('.md-block[data-line="6"]')).toContainText('Child item');
  await expect(desc.locator('.md-block[data-line="7"]')).toContainText('Grandchild item');
  await expect(desc.locator('.md-block[data-line="8"]')).toContainText('Sibling item');
  const listIndents = await desc.locator('.md-list-fragment').evaluateAll((lists) =>
    lists.map((list) => parseFloat(getComputedStyle(list).paddingInlineStart)));
  expect(listIndents[1]).toBeGreaterThan(listIndents[0]);
  expect(listIndents[2]).toBeGreaterThan(listIndents[1]);
  expect(listIndents[3]).toBe(listIndents[0]);
  await desc.locator('.desc-seg button[data-mdview="raw"]').click();
  await expect(desc.locator('.desc-raw')).toBeVisible();
  await expect(desc.locator('.md-preview')).toBeHidden();
  await expect(desc.locator('.desc-source .code').first()).toContainText('# Fixture PR');
  await desc.locator('.desc-seg button[data-mdview="preview"]').click();
  await expect(desc.locator('.md-preview')).toBeVisible();
  await expect(desc.locator('.desc-raw')).toBeHidden();
});

test('Preview/Diff expands a collapsed markdown file', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  const md = page.locator('#files-full details.file').filter({ has: page.locator('.md-seg') }).first();
  await md.evaluate((el) => { el.open = false; });
  await expect(md).not.toHaveAttribute('open', '');
  await md.locator('.md-seg button[data-mdview="diff"]').click();
  await expect(md).toHaveAttribute('open', '');
  await expect(md.locator('.diff-wrap')).toBeVisible();
  await md.evaluate((el) => { el.open = false; });
  await md.locator('.md-seg button[data-mdview="preview"]').click();
  await expect(md).toHaveAttribute('open', '');
  await expect(md.locator('.md-preview')).toBeVisible();
});

test('viewed state persists without a duplicate file-search sidebar', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  const cb = page.locator('#files-full details.file .file-viewed').first();
  await cb.check();
  await page.reload();
  await page.waitForFunction(() => document.querySelector('.file-viewed'));
  await expect(page.locator('#files-full details.file .file-viewed').first()).toBeChecked();
  await expect(page.locator('#file-search')).toHaveCount(0);
});

test('per-file wrapping and theme preference persist', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  const file = page.locator('#files-full details.file').first();
  await file.locator('.file-wrap-toggle').click();
  await expect(file).toHaveClass(/nowrap-lines/);
  await page.locator('#actions-menu').evaluate((el) => { el.open = true; });
  await page.locator('#theme-dark').click({ force: true });
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
});

test('commit range endpoint selection is visible and resettable', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  const rows = page.locator('tr.commit.selectable');
  if ((await rows.count()) < 2) {
    test.skip(true, 'no selectable commits in fixture');
    return;
  }
  await rows.nth(0).click();
  await rows.nth(1).click();
  await expect(page.locator('#range-bar')).toBeVisible();
  await expect(page.locator('#files-range .tok-kw').first()).toBeAttached();
  await page.locator('#range-reset').click();
  await expect(page.locator('#range-bar')).toBeHidden();
});

test('mobile keeps one navigation row and disables side-by-side', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 800 });
  await expect(page.locator('#sidebar-toggle')).toHaveCount(0);
  await expect(page.locator('#view-toggle')).toBeDisabled();
});

test('side-by-side preserves syntax highlighting markup', async ({ page }) => {
  await page.setViewportSize({ width: 1700, height: 900 });
  await expect(page.locator('#view-toggle')).toBeEnabled();
  await page.locator('#view-toggle').click();
  const python = page.locator('#files-full details.file[data-anchor="hello.py"]');
  await python.evaluate((el) => { el.open = true; el.scrollIntoView({ block: 'center' }); });
  await expect(python.locator('table.diff.split')).toBeVisible();
  await expect(python.locator('table.diff.split .tok-kw').first()).toBeAttached();
});

test('expand and collapse all files persist across restart', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  const openCount = () => page.locator('#files-full details.file[open]').count();
  const total = await page.locator('#files-full details.file').count();
  await page.locator('#actions-menu').evaluate((el) => { el.open = true; });
  await page.locator('#collapse-all').click({ force: true });
  expect(await openCount()).toBe(0);
  // Collapse state is deliberate view state: it survives a reload.
  await page.reload();
  await page.waitForFunction(() => {
    const el = document.getElementById('comment-count');
    return el && /comment/.test(el.textContent || '');
  });
  expect(await openCount()).toBe(0);
  await page.locator('#actions-menu').evaluate((el) => { el.open = true; });
  await page.locator('#expand-all').click({ force: true });
  expect(await openCount()).toBe(total);
});

test('keyboard help opens with ?', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  // Use an explicit non-typing focus target: clicking the document body can
  // legitimately land on a full-row comment target and open its editor.
  await page.locator('#review-back').focus();
  await page.keyboard.press('?');
  await expect(page.locator('#help-dialog')).toBeVisible();
  await page.locator('#help-close').click();
  await expect(page.locator('#help-dialog')).toBeHidden();
});

test('actions menu is keyboard complete', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.locator('#actions-menu summary').focus();
  await page.keyboard.press('ArrowDown');
  await expect(page.locator('#copy-md')).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(page.locator('#actions-menu summary')).toBeFocused();
});

test('mobile supports the complete create and cancel comment flow', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 800 });
  await page.evaluate(() => {
    const file = Array.from(document.querySelectorAll('#files-full details.file'))
      .find((d) => d.querySelector('.diff-wrap'));
    file.open = true;
    const preview = file.querySelector('.md-preview');
    if (preview) preview.hidden = true;
    file.querySelector('.diff-wrap').hidden = false;
  });
  const gutter = page.locator('#files-full .diff-wrap:not([hidden]) .gutter-btn').first();
  await gutter.click();
  const editor = page.locator('.pd-editor textarea');
  await editor.fill('mobile review');
  await page.keyboard.press('Control+Enter');
  await expect(page.locator('#comment-count')).toContainText('1 comment');

  await gutter.click();
  await expect(editor).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(editor).toBeHidden();
});

test('light and dark screenshots (layout smoke)', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  mkdirSync(join(root, 'tmp'), { recursive: true });
  await page.locator('#actions-menu').evaluate((el) => { el.open = true; });
  await page.locator('#theme-light').click({ force: true });
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await expect(page.locator('#topnav')).toBeVisible();
  await page.screenshot({ path: join(root, 'tmp', 'browser-light.png'), fullPage: false });
  await page.locator('#actions-menu').evaluate((el) => { el.open = true; });
  await page.locator('#theme-dark').click({ force: true });
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await page.screenshot({ path: join(root, 'tmp', 'browser-dark.png'), fullPage: false });
});

// ---- large-review safeguards: a perf budget the suite enforces ----------
// A deliberately large fixture (100 files × 80 changed lines). Regressions in
// page size or load-path work are invisible on the small fixture until a user
// hits them; these assertions are tripwires, not benchmarks.
test.describe('large review', () => {
  let large;

  test.beforeAll(() => {
    test.setTimeout(120_000);
    const dir = mkdtempSync(join(tmpdir(), 'packdiff-browser-large-'));
    const repo = join(dir, 'big');
    mkdirSync(repo);
    git(repo, ['init', '-q']);
    git(repo, ['symbolic-ref', 'HEAD', 'refs/heads/main']);
    write(repo, 'README.md', '# Big fixture\n');
    git(repo, ['add', '-A']);
    git(repo, ['commit', '-qm', 'initial']);
    git(repo, ['checkout', '-qb', 'feature']);
    const body = (i) => Array.from({ length: 80 }, (_, l) => `line ${l} of module ${i}: value = ${l * i};`).join('\n') + '\n';
    for (let i = 0; i < 100; i++) write(repo, `src/mod_${String(i).padStart(3, '0')}.txt`, body(i));
    git(repo, ['add', '-A']);
    git(repo, ['commit', '-qm', 'add one hundred modules']);
    for (let i = 0; i < 10; i++) write(repo, `src/mod_${String(i).padStart(3, '0')}.txt`, body(i) + 'tail change\n');
    git(repo, ['add', '-A']);
    git(repo, ['commit', '-qm', 'touch ten modules']);
    const out = join(dir, 'big-review.html');
    const r = spawnSync(findBinary(), ['main', 'feature', '-C', repo, '-o', out], { encoding: 'utf8' });
    if (r.status !== 0) throw new Error(`packdiff failed: ${r.stderr || r.stdout}`);
    large = { dir, out, fileUrl: pathToFileURL(out).href };
  });

  test.afterAll(() => {
    if (large?.dir) rmSync(large.dir, { recursive: true, force: true });
  });

  test('page byte size stays within budget; load reaches interactive promptly', async ({ page }) => {
    test.setTimeout(60_000);
    // ~8k changed lines should render to well under this; the budget trips on
    // catastrophic regressions (double-embedded payloads, unbounded chrome).
    expect(statSync(large.out).size).toBeLessThan(20 * 1024 * 1024);
    const t0 = Date.now();
    await page.goto(large.fileUrl);
    await page.waitForFunction(() => {
      const el = document.getElementById('comment-count');
      return el && /comment/.test(el.textContent || '');
    });
    expect(Date.now() - t0).toBeLessThan(15_000);
    // Navigation must work immediately: from the top, j lands on the next
    // panel (index 1). Click the summary text, not `body` — the page is
    // wall-to-wall diff lines and the whole line is a comment target.
    await page.locator('.review-summary-text').click();
    await page.keyboard.press('j');
    await expect(page.locator('#files-full details.file').nth(1)).toBeFocused();
  });

  test('split tables build lazily, near the viewport only', async ({ page }) => {
    test.setTimeout(60_000);
    await page.setViewportSize({ width: 1700, height: 900 });
    await page.goto(large.fileUrl);
    await page.waitForFunction(() => {
      const el = document.getElementById('comment-count');
      return el && /comment/.test(el.textContent || '');
    });
    await expect(page.locator('#view-toggle')).toBeEnabled();
    await page.locator('#view-toggle').click();
    // Panels build as they approach the viewport. Scroll into the diff (the
    // exact landing varies under content-visibility height estimates, so
    // assert the invariants, not a specific panel): something near the
    // viewport builds and swaps visible…
    await page.locator('#files-full details.file').first().evaluate((el) => el.scrollIntoView({ block: 'start' }));
    await page.waitForFunction(() => document.querySelectorAll('table.diff.split').length > 0);
    await expect(page.locator('table.diff.split').first()).toBeVisible();
    // …while distant panels stay unbuilt (the whole point of laziness).
    const built = await page.locator('table.diff.split').count();
    const total = await page.locator('#files-full details.file').count();
    expect(total).toBe(100);
    expect(built).toBeLessThan(total);
  });
});
