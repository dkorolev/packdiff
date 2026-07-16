'use strict';
// The PLAYER. packdiff splits the page in two — strict Rust for the ENGINE,
// vanilla JS for the PLAYER — a settled boundary, drawn by responsibility,
// not size (docs/ARCHITECTURE.md, "Web layer stance").
//
// This file owns presentation and browser state only: DOM assembly, event
// wiring, and view preferences (per-file wrap, theme, viewed files, drafts —
// a separate `…:prefs` localStorage record, never exported). Every
// review-document read/mutation/export goes through the embedded WASM build
// of packdiff-dto (`pd_*` calls). The litmus test for new code: if it would
// change what an export says or what a stored review document means, it
// belongs in Rust, not here. Framework-free and build-step-free by design.
(async function () {
  const CONFIG = JSON.parse(document.getElementById('packdiff-config').textContent);
  const META = JSON.stringify({ repo: CONFIG.repo, base: CONFIG.base, head: CONFIG.head });

  // ---- wasm bridge ----
  const b64 = document.getElementById('packdiff-wasm').textContent.trim();
  const raw = atob(b64);
  const bytes = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
  let ex;
  try {
    const { instance } = await WebAssembly.instantiate(bytes, {});
    ex = instance.exports;
  } catch (e) {
    const el = document.getElementById('wasm-error');
    el.textContent = 'Comment engine failed to load: ' + e;
    el.hidden = false;
    return;
  }
  const enc = new TextEncoder();
  const dec = new TextDecoder();
  function callWasm(name) {
    const inputs = Array.prototype.slice.call(arguments, 1);
    const args = [];
    const allocs = [];
    for (const s of inputs) {
      const b = enc.encode(s);
      const ptr = ex.pd_alloc(b.length);
      new Uint8Array(ex.memory.buffer, ptr, b.length).set(b);
      args.push(ptr, b.length);
      allocs.push([ptr, b.length]);
    }
    const packed = ex[name].apply(null, args);
    for (const [p, l] of allocs) ex.pd_free(p, l);
    const rptr = Number(packed >> 32n);
    const rlen = Number(packed & 0xffffffffn);
    const out = dec.decode(new Uint8Array(ex.memory.buffer, rptr, rlen).slice());
    ex.pd_free(rptr, rlen);
    const env = JSON.parse(out);
    if ('Ok' in env) return env.Ok;
    throw new Error(env.Error && env.Error.message ? env.Error.message : 'malformed engine response');
  }

  // The diff store is keyed by review identity AND schema generation: a page
  // only ever writes the generation its engine speaks. Older generations'
  // stores stay in place for the pages that wrote them and are absorbed
  // forward by engine merge (see initDoc) — so a stale page can never
  // clobber newer state by "starting fresh" over a document it cannot parse.
  const GEN = CONFIG.schema_version;
  const KEY = 'packdiff:v' + GEN + ':diff:' + CONFIG.review_id;
  const PREF_KEY = KEY + ':prefs';
  const ACT_KEY = KEY + ':activity';

  // The older stores this page absorbs from, newest generation first; the
  // pre-review_id SHA-pinned key (pd_storage_key) is the oldest.
  function olderStoreKeys() {
    const keys = [];
    for (let g = GEN - 1; g >= 1; g--) keys.push('packdiff:v' + g + ':diff:' + CONFIG.review_id);
    try { keys.push(callWasm('pd_storage_key', META)); } catch (e) { /* engine rejected meta */ }
    return keys;
  }

  // One-time adoption of view preferences and the undo journal from the
  // nearest older store (review DOCUMENTS are merged instead — see initDoc).
  try {
    if (localStorage.getItem(PREF_KEY) === null || localStorage.getItem(ACT_KEY) === null) {
      for (const src of olderStoreKeys()) {
        if (localStorage.getItem(PREF_KEY) === null) {
          const p = localStorage.getItem(src + ':prefs');
          if (p !== null) localStorage.setItem(PREF_KEY, p);
        }
        if (localStorage.getItem(ACT_KEY) === null) {
          const a = localStorage.getItem(src + ':activity');
          if (a !== null) localStorage.setItem(ACT_KEY, a);
        }
      }
    }
  } catch (e) { /* storage unavailable; initDoc() surfaces the warning */ }

  // ---- toast ----
  let toastTimer = 0;
  function showToast(message, isError) {
    const el = document.getElementById('toast');
    el.textContent = message;
    el.classList.toggle('error', !!isError);
    el.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { el.hidden = true; }, 8000);
  }
  function showError(message) { showToast(message, true); }
  document.getElementById('toast').addEventListener('click', (ev) => { ev.currentTarget.hidden = true; });
  document.getElementById('review-back').addEventListener('click', () => {
    // Review pages are normally opened from their owning job. Keep an explicit route
    // back in the page chrome; a standalone exported page simply closes when permitted.
    if (history.length > 1) history.back();
    else window.close();
  });
  // In-document navigation replaces the current URL instead of adding a
  // history entry: Back always leaves the review. Browser-tested in
  // "Back control returns to the page that opened the review".
  let explicitNavigationId = null;
  let explicitNavigationTimer = 0;
  document.addEventListener('click', (ev) => {
    const link = ev.target.closest('a[href^="#"]');
    if (!link) return;
    const id = link.getAttribute('href').slice(1);
    const target = document.getElementById(id);
    if (!target) return;
    ev.preventDefault();
    if (target.matches('details.file')) target.open = true;
    target.scrollIntoView({ block: 'start' });
    target.setAttribute('tabindex', '-1');
    target.focus({ preventScroll: true });
    explicitNavigationId = id;
    clearTimeout(explicitNavigationTimer);
    explicitNavigationTimer = setTimeout(() => { explicitNavigationId = null; }, 400);
    history.replaceState(null, '', '#' + id);
  });

  // ---- browser preferences (not part of the review document) ----
  const DEFAULT_PREFS = {
    schema_version: 1,
    viewed_files: [],
    nowrap_files: [],
    collapsed_files: [],
    markdown_views: {},
    theme: 'system',
    drafts: {},
    comment_hint_seen: false,
    absorbed: {},
  };
  let prefs = Object.assign({}, DEFAULT_PREFS, { viewed_files: [], drafts: {}, absorbed: {} });
  let prefsMemoryOnly = false;

  function loadPrefs() {
    try {
      const raw = localStorage.getItem(PREF_KEY);
      if (!raw) return;
      const parsed = JSON.parse(raw);
      if (!parsed || typeof parsed !== 'object') return;
      prefs = {
        schema_version: 1,
        viewed_files: Array.isArray(parsed.viewed_files)
          ? parsed.viewed_files.filter((p) => typeof p === 'string')
          : [],
        nowrap_files: Array.isArray(parsed.nowrap_files) ? parsed.nowrap_files : [],
        collapsed_files: Array.isArray(parsed.collapsed_files) ? parsed.collapsed_files : [],
        markdown_views: parsed.markdown_views && typeof parsed.markdown_views === 'object' ? parsed.markdown_views : {},
        theme: (parsed.theme === 'light' || parsed.theme === 'dark') ? parsed.theme : 'system',
        drafts: (parsed.drafts && typeof parsed.drafts === 'object') ? parsed.drafts : {},
        comment_hint_seen: !!parsed.comment_hint_seen,
        absorbed: (parsed.absorbed && typeof parsed.absorbed === 'object') ? parsed.absorbed : {},
      };
    } catch (e) {
      prefsMemoryOnly = true;
    }
  }
  function savePrefs() {
    if (prefsMemoryOnly) return;
    try {
      localStorage.setItem(PREF_KEY, JSON.stringify(prefs));
    } catch (e) {
      prefsMemoryOnly = true;
    }
  }
  function draftKey(anchor, editingId) {
    return anchor.file + ':' + anchor.side + ':' + anchor.line +
      (editingId ? ':edit:' + editingId : ':new');
  }
  function setDraft(anchor, editingId, text) {
    const k = draftKey(anchor, editingId);
    if (!text) {
      delete prefs.drafts[k];
    } else {
      prefs.drafts[k] = { text: text, editing: editingId || null };
    }
    savePrefs();
    if (doc) updateCount();
  }
  function getDraft(anchor, editingId) {
    const d = prefs.drafts[draftKey(anchor, editingId)];
    return d && typeof d.text === 'string' ? d.text : null;
  }
  function clearDraft(anchor, editingId) {
    setDraft(anchor, editingId, '');
  }

  function applyTheme() {
    if (prefs.theme === 'system') document.documentElement.removeAttribute('data-theme');
    else document.documentElement.setAttribute('data-theme', prefs.theme);
  }
  loadPrefs();
  applyTheme();

  // ---- storage (review document) ----
  let doc = null;
  let memoryOnly = false;
  function showStorageWarning() {
    const el = document.getElementById('storage-warning');
    if (el) el.hidden = false;
    memoryOnly = true;
  }
  // Tiny FNV-1a over a stored string — change detection for absorption only.
  function fnv(s) {
    let h = 0x811c9dc5;
    for (let i = 0; i < s.length; i++) {
      h ^= s.charCodeAt(i);
      h = Math.imul(h, 0x01000193);
    }
    return (h >>> 0).toString(16);
  }
  function initDoc() {
    let base = null;
    let raw = null;
    try { raw = localStorage.getItem(KEY); } catch (e) { showStorageWarning(); }
    if (raw !== null) {
      try { base = callWasm('pd_parse_document', raw); }
      catch (e) { console.warn('packdiff: stored document rejected (' + e.message + '); starting fresh'); }
    }
    if (!base) base = callWasm('pd_new_document', META);
    if (memoryOnly) return base;
    // Absorb older-generation stores by engine merge (union by id, newer
    // wins) — and re-absorb a source only when its content changed since the
    // last absorption, so a deletion made here is not resurrected by a
    // static old store. The old stores are never written or removed: the
    // pages that speak their schema keep working against them.
    let absorbed = false;
    for (const src of olderStoreKeys()) {
      let old = null;
      try { old = localStorage.getItem(src); } catch (e) { break; }
      if (old === null || prefs.absorbed[src] === fnv(old)) continue;
      try {
        base = callWasm('pd_merge', JSON.stringify(base), old);
        prefs.absorbed[src] = fnv(old);
        absorbed = true;
      } catch (e) {
        console.warn('packdiff: could not absorb ' + src + ' (' + e.message + ')');
      }
    }
    if (absorbed) {
      savePrefs();
      try { localStorage.setItem(KEY, JSON.stringify(base)); } catch (e) { showStorageWarning(); }
    }
    return base;
  }
  // ---- activity journal (material review changes; stable-ID operations) ----
  // Stored under its own key: it is review material, not a view preference,
  // and each entry is the operation (with what undo needs), never a document
  // snapshot.
  let activity = [];
  try { activity = JSON.parse(localStorage.getItem(ACT_KEY)) || []; } catch (e) { activity = []; }
  if (!Array.isArray(activity)) activity = [];
  // Older pages appended every verdict toggle. A review has at most one final
  // verdict action: compact the history to its latest non-cleared outcome.
  const verdictActivity = activity.filter((entry) => entry && entry.kind === 'verdict');
  const hadVerdictActivity = verdictActivity.length > 0;
  if (hadVerdictActivity) {
    const latest = verdictActivity[verdictActivity.length - 1];
    activity = activity.filter((entry) => !entry || entry.kind !== 'verdict');
    if (latest.label !== 'Verdict cleared') activity.push(latest);
  }
  function saveActivity() {
    if (memoryOnly) return;
    try { localStorage.setItem(ACT_KEY, JSON.stringify(activity)); } catch (e) { /* keep in memory */ }
  }
  if (hadVerdictActivity) saveActivity();
  function pushActivity(entry) {
    entry.id = genId();
    entry.at = new Date().toISOString();
    activity.push(entry);
    saveActivity();
  }

  function persist(next) {
    if (doc) {
      const before = {};
      for (const c of doc.comments) before[c.id] = c;
      const after = {};
      for (const c of next.comments) after[c.id] = c;
      for (const c of next.comments) {
        if (!before[c.id]) pushActivity({ kind: 'comment_added', label: 'Comment added', comment_id: c.id });
        else if (JSON.stringify(before[c.id]) !== JSON.stringify(c)) {
          const was = before[c.id];
          const label = !was.resolved_at && c.resolved_at ? 'Comment resolved'
            : was.resolved_at && !c.resolved_at ? 'Comment reopened' : 'Comment updated';
          pushActivity({ kind: 'comment_updated', label: label, comment: was });
        }
      }
      for (const c of doc.comments) {
        if (!after[c.id]) pushActivity({ kind: 'comment_deleted', label: 'Comment deleted', comment: c });
      }
    }
    doc = next;
    if (!memoryOnly) {
      try { localStorage.setItem(KEY, JSON.stringify(doc)); }
      catch (e) { showStorageWarning(); }
    }
    updateCount();
    syncViewedChecks();
    renderChanges();
  }
  function saveDoc(next) {
    persist(next);
    renderAll();
  }
  function genId() {
    return 'c' + Date.now().toString(36) + Math.random().toString(36).slice(2, 8);
  }

  // ---- anchoring ----
  function cssAttr(v) { return String(v).replace(/\\/g, '\\\\').replace(/"/g, '\\"'); }
  function anchorSel(prefix, c) {
    return prefix + '[data-file="' + cssAttr(c.file) + '"]' +
      '[data-side="' + cssAttr(c.side) + '"][data-line="' + cssAttr(String(c.line)) + '"]';
  }
  // Comment-anchor index: `file:side:line` → candidate elements, built once
  // per render pass instead of one full-DOM attribute query per comment
  // (which was O(comments × rows)). Split cells and unified rows are indexed
  // separately; visibility is still resolved per lookup, so the "prefer a
  // visible container" behavior is unchanged.
  let anchorIndex = null;
  function invalidateAnchorIndex() { anchorIndex = null; }
  function buildAnchorIndex() {
    anchorIndex = { rows: new Map(), cells: new Map() };
    const put = (map, el) => {
      const key = el.dataset.file + ':' + el.dataset.side + ':' + el.dataset.line;
      const list = map.get(key);
      if (list) list.push(el); else map.set(key, [el]);
    };
    document.querySelectorAll('tr.commentable').forEach((el) => put(anchorIndex.rows, el));
    document.querySelectorAll('td.code.commentable').forEach((el) => put(anchorIndex.cells, el));
  }
  function rowFor(c) {
    if (!anchorIndex) buildAnchorIndex();
    const key = c.file + ':' + c.side + ':' + String(c.line);
    if (document.body.classList.contains('view-split')) {
      const cells = anchorIndex.cells.get(key) || [];
      for (const cell of cells) {
        if (!cell.closest('[hidden]')) return cell.parentElement;
      }
      if (cells[0]) return cells[0].parentElement;
      // No split cell yet (split tables build lazily): fall back to the
      // unified row; comments re-place when this file's split is built.
    }
    // Prefer a row in a visible container (e.g. description Raw vs Preview).
    const rows = anchorIndex.rows.get(key) || [];
    for (const row of rows) {
      if (!row.closest('[hidden]')) return row;
    }
    return rows[0] || null;
  }
  function blockFor(c) {
    const blocks = document.querySelectorAll(
      '.md-preview:not([hidden]) .md-block[data-file="' + cssAttr(c.file) + '"]' +
      '[data-side="' + cssAttr(c.side) + '"]');
    let best = null;
    for (const b of blocks) {
      if (Number(b.dataset.line) <= Number(c.line) &&
          (!best || Number(b.dataset.line) >= Number(best.dataset.line))) best = b;
    }
    return best || blocks[0] || null;
  }
  function commentsBoxAfter(block) {
    let host = block.closest('.md-block-wrap') || block;
    let box = host.nextElementSibling;
    if (!box || !box.classList.contains('md-comments')) {
      box = document.createElement('div');
      box.className = 'md-comments';
      host.after(box);
    }
    return box;
  }
  function previewCardOf(id) {
    return Array.prototype.find.call(document.querySelectorAll('.md-comments .comment-card'),
      (el) => el.dataset.commentId === id) || null;
  }
  function placeComment(c) {
    const block = blockFor(c);
    if (block) {
      commentsBoxAfter(block).appendChild(commentCard(c));
      return true;
    }
    const row = rowFor(c);
    if (row) { insertCommentRow(row, c); return true; }
    return false;
  }

  // ---- counts / per-file meta ----
  function updateCount() {
    const n = doc.comments.length;
    const open = doc.comments.filter((c) => !c.resolved_at).length;
    const label = n + ' comment' + (n === 1 ? '' : 's') + (open === n ? '' : ' · ' + open + ' open');
    const btn = document.getElementById('comment-count');
    if (btn) { btn.textContent = label; btn.hidden = n === 0; }
    // gutter badges
    document.querySelectorAll('.gutter-btn').forEach((btn) => {
      const file = btn.dataset.file;
      const side = btn.dataset.side;
      const line = Number(btn.dataset.line);
      if (!file) return;
      const count = doc.comments.filter((c) =>
        c.file === file && c.side === side && Number(c.line) === line).length;
      if (count > 0) {
        btn.classList.add('has-comments');
        btn.textContent = String(count);
        btn.setAttribute('aria-label', count + ' comment' + (count === 1 ? '' : 's') +
          ' on ' + side + ' line ' + line + '; add another');
      } else {
        btn.classList.remove('has-comments');
        btn.textContent = '+';
        btn.setAttribute('aria-label', 'Add comment on ' + side + ' line ' + line);
      }
    });
    // per-file comment counts on headers
    document.querySelectorAll('details.file').forEach((panel) => {
      const anchor = panel.dataset.anchor;
      if (!anchor) return;
      const n = doc.comments.filter((c) => c.file === anchor).length;
      const el = panel.querySelector('[data-role="file-comment-count"]');
      if (el) {
        if (n > 0) {
          el.hidden = false;
          el.textContent = n + ' comment' + (n === 1 ? '' : 's');
        } else {
          el.hidden = true;
          el.textContent = '';
        }
      }
      const drafts = Object.keys(prefs.drafts).filter((key) => key.indexOf(anchor + ':') === 0).length;
      const draftEl = panel.querySelector('[data-role="file-draft-count"]');
      if (draftEl) {
        draftEl.hidden = drafts === 0;
        draftEl.textContent = drafts ? drafts + ' draft' + (drafts === 1 ? '' : 's') : '';
      }
    });
    const context = document.getElementById('current-file-context');
    if (context && context.dataset.fileId) {
      const current = document.getElementById(context.dataset.fileId);
      context.dataset.fileId = '';
      renderCurrentFile(current);
    }
  }

  function syncViewedChecks() {
    document.querySelectorAll('.file-viewed').forEach((cb) => {
      cb.checked = prefs.viewed_files.indexOf(cb.dataset.anchor) >= 0;
    });
  }

  function renderAll() {
    invalidateAnchorIndex();
    document.querySelectorAll('tr.comment-row').forEach((el) => el.remove());
    document.querySelectorAll('.md-comments .comment-card').forEach((el) => el.remove());
    document.querySelectorAll('.md-comments').forEach((el) => { if (!el.children.length) el.remove(); });
    const un = document.getElementById('unanchored');
    un.hidden = true;
    un.querySelectorAll('.comment-card').forEach((el) => el.remove());
    for (const c of doc.comments) {
      if (!placeComment(c)) un.appendChild(commentCard(c));
    }
    if (un.querySelector('.comment-card')) un.hidden = false;
    updateCount();
    syncViewedChecks();
  }

  function formatTimestamp(value) {
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) return value || '';
    try {
      return new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' }).format(date);
    } catch (e) {
      return date.toLocaleString();
    }
  }

  function focusCommentById(id) {
    const card = Array.prototype.find.call(document.querySelectorAll('.comment-card'),
      (el) => el.dataset.commentId === id) || null;
    if (!card) { showToast('That comment is no longer present'); return; }
    const file = card.closest('details.file');
    if (file) file.open = true;
    card.scrollIntoView({ block: 'center' });
    card.setAttribute('tabindex', '-1');
    card.focus({ preventScroll: true });
    card.classList.add('arrival-flash');
    setTimeout(() => card.classList.remove('arrival-flash'), 950);
    if (file && file.id) history.replaceState(null, '', '#' + file.id);
  }

  function renderChanges() {
    const count = document.getElementById('change-count');
    const undo = document.getElementById('undo-change');
    const body = document.getElementById('changes-body');
    const entries = activity;
    if (count) {
      count.hidden = entries.length === 0;
      count.textContent = String(entries.length);
      count.setAttribute('aria-label', entries.length + ' review change' + (entries.length === 1 ? '' : 's'));
    }
    if (undo) undo.disabled = entries.length === 0;
    if (!body) return;
    body.textContent = '';
    if (!entries.length) { body.textContent = 'No review changes yet.'; return; }
    const list = document.createElement('ol');
    list.className = 'change-list';
    entries.slice().reverse().forEach((entry) => {
      const item = document.createElement('li');
      const commentId = entry.comment_id || (entry.comment && entry.comment.id);
      const currentComment = commentId && doc.comments.some((comment) => comment.id === commentId);
      if (currentComment) {
        const link = document.createElement('a');
        link.href = '#changes';
        link.textContent = entry.label;
        link.addEventListener('click', (ev) => {
          ev.preventDefault();
          ev.stopPropagation();
          focusCommentById(commentId);
        });
        item.appendChild(link);
      } else {
        item.appendChild(document.createTextNode(entry.label));
      }
      const time = document.createElement('time');
      time.dateTime = entry.at;
      time.title = entry.at;
      time.textContent = formatTimestamp(entry.at);
      item.appendChild(time);
      list.appendChild(item);
    });
    body.appendChild(list);
  }
  document.getElementById('undo-change').addEventListener('click', () => {
    const entry = activity.pop();
    if (!entry) return;
    if (entry.kind === 'viewed') {
      const set = new Set(prefs.viewed_files);
      if (entry.viewed) set.delete(entry.anchor); else set.add(entry.anchor);
      prefs.viewed_files = Array.from(set);
      savePrefs();
    } else {
      // Invert the journal operation through the same engine calls that made it.
      let next;
      try {
        if (entry.kind === 'comment_added') next = callWasm('pd_delete_comment', JSON.stringify(doc), entry.comment_id);
        else if (entry.kind === 'verdict') next = callWasm('pd_set_verdict', JSON.stringify(doc), 'null');
        else next = callWasm('pd_upsert_comment', JSON.stringify(doc), JSON.stringify(entry.comment));
      } catch (e) {
        activity.push(entry);
        showError('Undo failed: ' + e.message);
        return;
      }
      doc = next;
      if (!memoryOnly) {
        try { localStorage.setItem(KEY, JSON.stringify(doc)); } catch (e) { showStorageWarning(); }
      }
    }
    saveActivity();
    renderVerdict();
    renderAll(); renderChanges();
    showToast('Last review change undone');
  });

  // ---- review verdict ----
  // Comment is the no-verdict state. Approved and ChangesRequired each occupy
  // exactly one final journal action; changing the outcome replaces it.
  function renderVerdict() {
    const comment = document.getElementById('verdict-comment');
    const approve = document.getElementById('verdict-approve');
    const changes = document.getElementById('verdict-changes');
    if (!comment || !approve || !changes) return;
    const key = doc && doc.verdict ? Object.keys(doc.verdict)[0] : null;
    comment.setAttribute('aria-pressed', key === null ? 'true' : 'false');
    approve.setAttribute('aria-pressed', key === 'Approved' ? 'true' : 'false');
    changes.setAttribute('aria-pressed', key === 'ChangesRequired' ? 'true' : 'false');
  }
  function setVerdict(variant) {
    const current = doc.verdict ? Object.keys(doc.verdict)[0] : null;
    if (current === variant) return;
    const target = variant === null ? null : {};
    if (target) target[variant] = { at: new Date().toISOString() };
    let next;
    try { next = callWasm('pd_set_verdict', JSON.stringify(doc), JSON.stringify(target)); }
    catch (e) { showError('Verdict failed: ' + e.message); return; }
    activity = activity.filter((entry) => !entry || entry.kind !== 'verdict');
    saveActivity();
    if (target) pushActivity({
      kind: 'verdict',
      label: variant === 'Approved' ? 'Change approved' : 'Changes required',
    });
    persist(next);
    renderVerdict();
  }
  document.getElementById('verdict-comment').addEventListener('click', () => setVerdict(null));
  document.getElementById('verdict-approve').addEventListener('click', () => setVerdict('Approved'));
  document.getElementById('verdict-changes').addEventListener('click', () => setVerdict('ChangesRequired'));

  function commentCard(c) {
    const wrap = document.createElement('div');
    wrap.className = 'comment-card';
    if (c.resolved_at) wrap.classList.add('resolved');
    wrap.dataset.commentId = c.id;
    const meta = document.createElement('div');
    meta.className = 'comment-meta';
    meta.appendChild(document.createTextNode(c.file + ' · ' + c.side.toLowerCase() + ' line ' + c.line + ' · '));
    const updated = document.createElement('time');
    updated.dateTime = c.updated_at;
    updated.title = c.updated_at;
    updated.textContent = formatTimestamp(c.updated_at);
    meta.appendChild(updated);
    if (c.resolved_at) {
      meta.appendChild(document.createTextNode(' · resolved '));
      const resolved = document.createElement('time');
      resolved.dateTime = c.resolved_at;
      resolved.title = c.resolved_at;
      resolved.textContent = formatTimestamp(c.resolved_at);
      meta.appendChild(resolved);
    }
    const body = document.createElement('div');
    body.className = 'comment-body';
    try { body.innerHTML = callWasm('pd_markdown_html', c.text); }
    catch (e) { body.textContent = c.text; }
    const actions = document.createElement('div');
    actions.className = 'comment-actions';
    const edit = document.createElement('button');
    edit.type = 'button';
    edit.textContent = 'Edit';
    edit.addEventListener('click', () => openEditorFor(c));
    // Resolve/reopen is an ordinary engine upsert: resolved_at rides the
    // comment, and the updated_at bump lets merges pick the newer state.
    const resolve = document.createElement('button');
    resolve.type = 'button';
    resolve.textContent = c.resolved_at ? 'Reopen' : 'Resolve';
    resolve.addEventListener('click', () => {
      const now = new Date().toISOString();
      const updated = Object.assign({}, c, { updated_at: now, resolved_at: c.resolved_at ? undefined : now });
      let next;
      try { next = callWasm('pd_upsert_comment', JSON.stringify(doc), JSON.stringify(updated)); }
      catch (e) { showError((c.resolved_at ? 'Reopen' : 'Resolve') + ' failed: ' + e.message); return; }
      persist(next);
      removeCommentDom(c.id);
      const fresh = doc.comments.find((x) => x.id === c.id);
      if (fresh) placeComment(fresh);
    });
    const del = document.createElement('button');
    del.type = 'button';
    del.textContent = 'Delete';
    del.addEventListener('click', () => {
      if (del.dataset.confirm !== 'true') {
        del.dataset.confirm = 'true';
        del.textContent = 'Confirm delete';
        del.classList.add('danger');
        return;
      }
      let next;
      try { next = callWasm('pd_delete_comment', JSON.stringify(doc), c.id); }
      catch (e) { showError('Delete failed: ' + e.message); return; }
      persist(next);
      removeCommentDom(c.id);
    });
    actions.appendChild(edit);
    actions.appendChild(resolve);
    actions.appendChild(del);
    wrap.appendChild(meta);
    wrap.appendChild(body);
    wrap.appendChild(actions);
    return wrap;
  }

  function buildCommentRow(c, colSpan) {
    const tr = document.createElement('tr');
    tr.className = 'comment-row';
    tr.dataset.commentId = c.id;
    const td = document.createElement('td');
    td.colSpan = colSpan;
    td.appendChild(commentCard(c));
    tr.appendChild(td);
    return tr;
  }
  function afterComments(row) {
    let after = row;
    while (after.nextElementSibling &&
           after.nextElementSibling.classList.contains('comment-row')) {
      after = after.nextElementSibling;
    }
    return after;
  }
  function insertCommentRow(row, c) {
    afterComments(row).after(buildCommentRow(c, row.cells ? row.cells.length : 4));
  }
  function commentRowOf(id) {
    return Array.prototype.find.call(
      document.querySelectorAll('tr.comment-row'), (tr) => tr.dataset.commentId === id) || null;
  }
  function removeCommentDom(id) {
    const row = commentRowOf(id);
    if (row) row.remove();
    document.querySelectorAll('.md-comments .comment-card, #unanchored .comment-card').forEach((el) => {
      if (el.dataset.commentId === id) el.remove();
    });
    document.querySelectorAll('.md-comments').forEach((el) => { if (!el.children.length) el.remove(); });
    const un = document.getElementById('unanchored');
    if (!un.querySelector('.comment-card')) un.hidden = true;
    updateCount();
    syncViewedChecks();
  }

  // ---- editor ----
  function closeEditor(host) {
    if (host.dataset.editing) {
      const row = commentRowOf(host.dataset.editing);
      if (row) row.style.display = '';
      const card = previewCardOf(host.dataset.editing);
      if (card) card.style.display = '';
    }
    const box = host.parentElement;
    host.remove();
    if (box && box.classList.contains('md-comments') && !box.children.length) box.remove();
  }
  function closeEditors() {
    document.querySelectorAll('.pd-editor').forEach((el) => closeEditor(el));
  }
  function sameAnchor(editor, anchor) {
    return anchor && editor.dataset.file === anchor.file && editor.dataset.side === anchor.side &&
      editor.dataset.line === String(anchor.line);
  }
  function closeEmptyNewEditors(exceptAnchor) {
    document.querySelectorAll('.pd-editor:not([data-editing])').forEach((editor) => {
      const textarea = editor.querySelector('textarea');
      if (!textarea || textarea.value.trim() || sameAnchor(editor, exceptAnchor)) return;
      const anchor = { file: editor.dataset.file, side: editor.dataset.side, line: editor.dataset.line };
      clearDraft(anchor, null);
      closeEditor(editor);
    });
  }
  function findEditor(anchor, editingId) {
    return Array.prototype.find.call(document.querySelectorAll('.pd-editor'), (el) =>
      el.dataset.file === anchor.file && el.dataset.side === anchor.side &&
      el.dataset.line === String(anchor.line) &&
      (el.dataset.editing || '') === (editingId || '')) || null;
  }
  function focusExisting(anchor, existing) {
    const open = findEditor(anchor, existing ? existing.id : null);
    if (open) { open.querySelector('textarea').focus(); return true; }
    return false;
  }

  function autosize(ta) {
    ta.style.height = 'auto';
    const h = Math.min(280, Math.max(70, ta.scrollHeight));
    ta.style.height = h + 'px';
  }

  function buildEditor(host, parent, anchor, existing) {
    host.classList.add('pd-editor');
    host.dataset.file = anchor.file;
    host.dataset.side = anchor.side;
    host.dataset.line = String(anchor.line);
    if (existing) host.dataset.editing = existing.id;

    const header = document.createElement('div');
    header.className = 'editor-header';
    header.textContent = (existing ? 'Edit comment on ' : 'Comment on ') +
      anchor.file + ' · ' + anchor.side.toLowerCase() + ' line ' + anchor.line;

    const tabs = document.createElement('div');
    tabs.className = 'seg editor-tabs';
    const tabWrite = document.createElement('button');
    tabWrite.type = 'button';
    tabWrite.textContent = 'Write';
    tabWrite.className = 'active';
    tabWrite.setAttribute('aria-pressed', 'true');
    const tabPreview = document.createElement('button');
    tabPreview.type = 'button';
    tabPreview.textContent = 'Preview';
    tabPreview.setAttribute('aria-pressed', 'false');
    tabs.appendChild(tabWrite);
    tabs.appendChild(tabPreview);

    const ta = document.createElement('textarea');
    ta.placeholder = 'Leave a comment… (markdown supported)';
    ta.setAttribute('aria-label', 'Comment body');
    const draft = getDraft(anchor, existing ? existing.id : null);
    if (draft !== null) ta.value = draft;
    else if (existing) ta.value = existing.text;

    if (!prefs.comment_hint_seen) {
      const hint = document.createElement('p');
      hint.className = 'comment-hint';
      hint.textContent = 'Tip: comments are markdown. Ctrl/Cmd+Enter saves; Esc cancels.';
      parent.appendChild(header);
      parent.appendChild(hint);
      prefs.comment_hint_seen = true;
      savePrefs();
    } else {
      parent.appendChild(header);
    }
    parent.appendChild(tabs);
    parent.appendChild(ta);

    const preview = document.createElement('div');
    preview.className = 'editor-preview';
    preview.hidden = true;
    parent.appendChild(preview);

    const actions = document.createElement('div');
    actions.className = 'editor-actions';
    const save = document.createElement('button');
    save.type = 'button';
    save.className = 'primary';
    save.textContent = existing ? 'Save changes' : 'Add comment';
    const cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.textContent = 'Cancel';
    const hint = document.createElement('span');
    hint.className = 'editor-hint';
    hint.textContent = 'Ctrl/Cmd+Enter to save · Esc to cancel';
    actions.appendChild(save);
    actions.appendChild(cancel);
    actions.appendChild(hint);
    parent.appendChild(actions);

    function showWrite() {
      ta.hidden = false;
      preview.hidden = true;
      tabWrite.classList.add('active');
      tabWrite.setAttribute('aria-pressed', 'true');
      tabPreview.classList.remove('active');
      tabPreview.setAttribute('aria-pressed', 'false');
    }
    function showPreview() {
      try { preview.innerHTML = callWasm('pd_markdown_html', ta.value); }
      catch (e) { preview.textContent = ta.value; }
      ta.hidden = true;
      preview.hidden = false;
      tabPreview.classList.add('active');
      tabPreview.setAttribute('aria-pressed', 'true');
      tabWrite.classList.remove('active');
      tabWrite.setAttribute('aria-pressed', 'false');
    }
    tabWrite.addEventListener('click', showWrite);
    tabPreview.addEventListener('click', showPreview);

    ta.addEventListener('input', () => {
      autosize(ta);
      setDraft(anchor, existing ? existing.id : null, ta.value);
    });
    autosize(ta);

    function doSave() {
      const text = ta.value.trim();
      if (!text) {
        clearDraft(anchor, existing ? existing.id : null);
        closeEditor(host);
        return;
      }
      const now = new Date().toISOString();
      const comment = existing
        ? Object.assign({}, existing, { text: text, updated_at: now })
        : { id: genId(), file: anchor.file, side: anchor.side,
            line: Number(anchor.line), text: text,
            created_at: now, updated_at: now };
      let next;
      try {
        next = callWasm('pd_upsert_comment', JSON.stringify(doc), JSON.stringify(comment));
      } catch (e) {
        showError('Comment rejected: ' + e.message);
        return;
      }
      clearDraft(anchor, existing ? existing.id : null);
      persist(next);
      if (existing) removeCommentDom(existing.id);
      closeEditor(host);
      placeComment(comment);
      updateCount();
      syncViewedChecks();
    }
    save.addEventListener('click', doSave);
    cancel.addEventListener('click', () => {
      clearDraft(anchor, existing ? existing.id : null);
      closeEditor(host);
    });
    ta.addEventListener('keydown', (ev) => {
      if ((ev.ctrlKey || ev.metaKey) && ev.key === 'Enter') { ev.preventDefault(); doSave(); }
      if (ev.key === 'Escape') {
        ev.preventDefault();
        clearDraft(anchor, existing ? existing.id : null);
        closeEditor(host);
      }
    });
    return ta;
  }

  function openTableEditor(afterRow, anchor, existing) {
    if (focusExisting(anchor, existing)) return;
    const tr = document.createElement('tr');
    tr.className = 'editor-row';
    const td = document.createElement('td');
    td.colSpan = afterRow.cells ? afterRow.cells.length : 4;
    tr.appendChild(td);
    const ta = buildEditor(tr, td, anchor, existing);
    if (existing) {
      const row = commentRowOf(existing.id);
      if (!row) return;
      row.style.display = 'none';
      row.after(tr);
    } else {
      afterComments(afterRow).after(tr);
    }
    ta.focus();
  }
  function openPreviewEditor(block, anchor, existing) {
    if (focusExisting(anchor, existing)) return;
    const div = document.createElement('div');
    div.className = 'editor-box';
    const ta = buildEditor(div, div, anchor, existing);
    if (existing) {
      const card = previewCardOf(existing.id);
      if (!card) return;
      card.style.display = 'none';
      card.after(div);
    } else {
      commentsBoxAfter(block).appendChild(div);
    }
    ta.focus();
  }
  function openEditorFor(c) {
    const anchor = { file: c.file, side: c.side, line: c.line };
    if (previewCardOf(c.id)) { openPreviewEditor(null, anchor, c); return; }
    const row = rowFor(c);
    if (row) openTableEditor(row, anchor, c);
  }

  function openNewComment(anchor, hostRowOrBlock) {
    if (hostRowOrBlock && hostRowOrBlock.classList && hostRowOrBlock.classList.contains('md-block')) {
      openPreviewEditor(hostRowOrBlock, anchor, null);
    } else if (hostRowOrBlock && hostRowOrBlock.tagName === 'TR') {
      openTableEditor(hostRowOrBlock, anchor, null);
    } else {
      const row = rowFor(anchor);
      if (row) openTableEditor(row, anchor, null);
      else {
        const block = blockFor(anchor);
        if (block) openPreviewEditor(block, anchor, null);
      }
    }
  }

  // The gutter is a visual affordance; the entire valid line is the target.
  document.addEventListener('click', (ev) => {
    const gbtn = ev.target.closest('.gutter-btn');
    const row = ev.target.closest('tr.commentable');
    const blockEl = ev.target.closest('.md-block');
    if (!gbtn && !row && !blockEl) {
      if (!ev.target.closest('.pd-editor')) closeEmptyNewEditors(null);
      return;
    }
    if (!gbtn && ev.target.closest('button, a, input, textarea, .comment-card, .pd-editor')) {
      closeEmptyNewEditors(null);
      return;
    }
    ev.preventDefault();
    ev.stopPropagation();
    const source = gbtn || row || blockEl;
    const anchor = {
      file: source.dataset.file,
      side: source.dataset.side,
      line: source.dataset.line,
    };
    if (!anchor.file) return;
    closeEmptyNewEditors(anchor);
    if (source.closest('#files-range')) {
      showToast('The range view is read-only — comments attach to the full diff. Click “Show full diff” to comment.');
      return;
    }
    const block = blockEl || (gbtn && gbtn.closest('.md-block-wrap')
      ? gbtn.closest('.md-block-wrap').querySelector('.md-block')
      : null);
    if (block) openNewComment(anchor, block);
    else {
      openNewComment(anchor, row || gbtn.closest('tr'));
    }
  });

  // Markdown | Source for both markdown files and the description panel.
  // On file panels the pill lives inside <summary>: expand the panel and
  // stop the default <details> toggle dance.
  document.addEventListener('click', (ev) => {
    const btn = ev.target.closest('.md-seg button');
    if (!btn) return;
    ev.preventDefault();
    ev.stopPropagation();
    const file = btn.closest('details.file');
    const desc = btn.closest('#description');
    const host = file || desc;
    if (!host) return;
    if (file) file.open = true;
    const preview = host.querySelector('.md-preview');
    const alt = file
      ? host.querySelector('.diff-wrap')
      : host.querySelector('.desc-raw');
    if (!preview || !alt) return;
    const showPreview = btn.dataset.mdview === 'preview';
    const scrollY = window.scrollY;
    const drafts = captureDrafts();
    closeEditors();
    preview.hidden = !showPreview;
    alt.hidden = showPreview;
    btn.closest('.md-seg').querySelectorAll('button').forEach((b) => {
      const on = b === btn;
      b.classList.toggle('active', on);
      b.setAttribute('aria-pressed', on ? 'true' : 'false');
    });
    const key = file ? file.dataset.anchor : '__description__';
    prefs.markdown_views[key] = showPreview ? 'rendered' : 'source';
    savePrefs();
    renderAll();
    restoreDrafts(drafts);
    window.scrollTo(0, scrollY);
  });

  document.addEventListener('change', (ev) => {
    const cb = ev.target.closest('.file-viewed');
    if (!cb) return;
    const anchor = cb.dataset.anchor;
    const set = new Set(prefs.viewed_files);
    if (cb.checked) set.add(anchor);
    else set.delete(anchor);
    prefs.viewed_files = Array.from(set);
    pushActivity({ kind: 'viewed', label: cb.checked ? 'File marked viewed' : 'File marked unviewed', anchor: anchor, viewed: cb.checked });
    savePrefs();
    syncViewedChecks();
    renderChanges();
  });
  document.addEventListener('toggle', (ev) => {
    const file = ev.target.closest && ev.target.closest('details.file');
    if (!file || ev.target !== file) return;
    const set = new Set(prefs.collapsed_files);
    if (file.open) set.delete(file.dataset.anchor); else set.add(file.dataset.anchor);
    prefs.collapsed_files = Array.from(set);
    savePrefs();
  }, true);
  document.addEventListener('click', (ev) => {
    const count = ev.target.closest('.file-comment-count');
    if (!count) return;
    ev.preventDefault(); ev.stopPropagation();
    const file = count.closest('details.file');
    file.open = true;
    const first = Array.prototype.find.call(document.querySelectorAll(
      'tr.comment-row, .md-comments .comment-card'), (el) => el.closest('details.file') === file);
    if (first) {
      const target = first.querySelector('.comment-card') || first;
      first.scrollIntoView({ block: 'center' });
      target.setAttribute('tabindex', '-1'); target.focus({ preventScroll: true });
      target.classList.add('arrival-flash');
      setTimeout(() => target.classList.remove('arrival-flash'), 950);
    }
  });

  // ---- commit range filtering ----
  const snapTag = document.getElementById('packdiff-snapshots');
  const SNAPSHOTS = snapTag ? snapTag.textContent : null;
  let rangeFrom = null, rangeTo = null;

  function setRange(from, to) {
    rangeFrom = from;
    rangeTo = to;
    applyRange();
  }

  document.addEventListener('click', (ev) => {
    const copy = ev.target.closest('button.copy-sha');
    if (copy) {
      copyText(copy.closest('tr').dataset.sha, copy);
      return;
    }
    const row = ev.target.closest('tr.commit.selectable');
    if (!row || !SNAPSHOTS) return;
    const idx = Number(row.dataset.index);
    if (rangeFrom !== null && rangeFrom === rangeTo && idx !== rangeFrom) {
      setRange(Math.min(rangeFrom, idx), Math.max(rangeTo, idx));
    } else if (rangeFrom === idx && rangeTo === idx) {
      setRange(null, null);
    } else {
      setRange(idx, idx);
    }
  });
  let dragRangeStart = null;
  let dragRangeEnd = null;
  function previewRange(from, to) {
    const lo = Math.min(from, to), hi = Math.max(from, to);
    document.querySelectorAll('tr.commit.selectable').forEach((row) => {
      const index = Number(row.dataset.index);
      row.classList.toggle('range-preview', index >= lo && index <= hi);
    });
  }
  document.addEventListener('pointerdown', (ev) => {
    const row = ev.target.closest('tr.commit.selectable');
    if (!row || ev.target.closest('button')) return;
    dragRangeStart = Number(row.dataset.index);
    dragRangeEnd = dragRangeStart;
  });
  document.addEventListener('pointerover', (ev) => {
    if (dragRangeStart === null) return;
    const row = ev.target.closest('tr.commit.selectable');
    if (!row) return;
    dragRangeEnd = Number(row.dataset.index);
    previewRange(dragRangeStart, dragRangeEnd);
  });
  document.addEventListener('pointerup', (ev) => {
    if (dragRangeStart === null) return;
    const start = dragRangeStart, end = dragRangeEnd;
    dragRangeStart = dragRangeEnd = null;
    document.querySelectorAll('.range-preview').forEach((row) => row.classList.remove('range-preview'));
    if (start !== end) {
      ev.preventDefault();
      setRange(Math.min(start, end), Math.max(start, end));
      history.replaceState({ range: [Math.min(start, end), Math.max(start, end)] }, '', '#commits');
    }
  });
  const rangeReset = document.getElementById('range-reset');
  if (rangeReset) rangeReset.addEventListener('click', () => setRange(null, null));
  const rangePrev = document.getElementById('range-prev');
  const rangeNext = document.getElementById('range-next');
  if (rangePrev) rangePrev.addEventListener('click', () => {
    const n = document.querySelectorAll('tr.commit.selectable').length;
    if (!n) return;
    if (rangeFrom === null) setRange(n, n);
    else setRange(Math.max(1, rangeFrom - 1), Math.max(1, rangeFrom - 1));
  });
  if (rangeNext) rangeNext.addEventListener('click', () => {
    const n = document.querySelectorAll('tr.commit.selectable').length;
    if (!n) return;
    if (rangeFrom === null) setRange(1, 1);
    else setRange(Math.min(n, rangeTo + 1), Math.min(n, rangeTo + 1));
  });

  function applyRange() {
    const bar = document.getElementById('range-bar');
    const fullFiles = document.getElementById('files-full');
    const rangedFiles = document.getElementById('files-range');
    const fullList = document.getElementById('filelist-full');
    const rangedList = document.getElementById('filelist-range');
    document.querySelectorAll('tr.commit').forEach((tr) => {
      const i = Number(tr.dataset.index);
      const on = rangeFrom !== null && i >= rangeFrom && i <= rangeTo;
      tr.classList.toggle('selected', on);
      tr.setAttribute('aria-selected', on ? 'true' : 'false');
    });
    if (rangeFrom === null) {
      if (bar) bar.hidden = true;
      rangedFiles.hidden = true;
      rangedFiles.textContent = '';
      if (rangedList) { rangedList.hidden = true; rangedList.textContent = ''; }
      fullFiles.style.display = '';
      if (fullList) fullList.style.display = '';
      return;
    }
    let files;
    try {
      files = callWasm('pd_range_diff', SNAPSHOTS,
        JSON.stringify({ from: rangeFrom - 1, to: rangeTo, context: 3 }));
    } catch (e) {
      showError('Range diff failed: ' + e.message);
      return;
    }
    rangedFiles.textContent = '';
    if (rangedList) rangedList.textContent = '';
    if (files.length === 0) {
      const p = document.createElement('p');
      p.className = 'muted';
      p.textContent = 'No changes in the selected commits.';
      rangedFiles.appendChild(p);
      if (rangedList) rangedList.appendChild(p.cloneNode(true));
    }
    let adds = 0, dels = 0;
    const list = document.createElement('table');
    list.className = 'filelist';
    files.forEach((f, i) => {
      rangedFiles.appendChild(rangeFileEl(f, i));
      list.appendChild(rangeFileListRow(f, i));
      adds += f.additions;
      dels += f.deletions;
    });
    if (files.length && rangedList) rangedList.appendChild(list);
    const shortOf = (i) =>
      document.querySelector('tr.commit[data-index="' + i + '"] code').textContent;
    const n = rangeTo - rangeFrom + 1;
    const total = document.querySelectorAll('tr.commit.selectable').length;
    document.getElementById('range-label').textContent = n === 1
      ? 'Showing commit ' + shortOf(rangeFrom) + ' only (' + rangeFrom + ' of ' + total + ')'
      : 'Showing commits ' + rangeFrom + '–' + rangeTo + ' of ' + total +
        ' (' + shortOf(rangeFrom) + '..' + shortOf(rangeTo) + ')';
    if (bar) bar.hidden = false;
    fullFiles.style.display = 'none';
    if (fullList) fullList.style.display = 'none';
    rangedFiles.hidden = false;
    if (rangedList) rangedList.hidden = false;
    if (document.body.classList.contains('view-split')) applyDiffView();
  }

  function rangeFileListRow(f, i) {
    const tr = document.createElement('tr');
    const path = document.createElement('td');
    path.className = 'fl-path';
    if (f.status !== 'Modified') {
      const badge = document.createElement('span');
      badge.className = 'badge badge-' + f.status.toLowerCase();
      badge.textContent = f.status.toLowerCase();
      path.appendChild(badge);
      path.appendChild(document.createTextNode(' '));
    }
    const a = document.createElement('a');
    a.href = '#rfile-' + i;
    a.textContent = f.old_path && f.new_path && f.old_path !== f.new_path
      ? f.old_path + ' → ' + f.new_path : (f.new_path || f.old_path);
    path.appendChild(a);
    const stats = document.createElement('td');
    stats.className = 'fl-stats';
    const adds = document.createElement('span');
    adds.className = 'adds';
    adds.textContent = '+' + f.additions;
    const dels = document.createElement('span');
    dels.className = 'dels';
    dels.textContent = '−' + f.deletions;
    stats.appendChild(adds);
    stats.appendChild(document.createTextNode(' '));
    stats.appendChild(dels);
    tr.appendChild(path);
    tr.appendChild(stats);
    return tr;
  }

  function rangeFileEl(f, i) {
    const details = document.createElement('details');
    details.className = 'file';
    details.id = 'rfile-' + i;
    details.open = true;
    const summary = document.createElement('summary');
    summary.className = 'file-summary';
    const left = document.createElement('span');
    left.className = 'file-left';
    if (f.status !== 'Modified') {
      const badge = document.createElement('span');
      badge.className = 'badge badge-' + f.status.toLowerCase();
      badge.textContent = f.status.toLowerCase();
      left.appendChild(badge);
      left.appendChild(document.createTextNode(' '));
    }
    const path = document.createElement('span');
    path.className = 'path';
    path.textContent = f.old_path && f.new_path && f.old_path !== f.new_path
      ? f.old_path + ' → ' + f.new_path : (f.new_path || f.old_path);
    left.appendChild(path);
    summary.appendChild(left);
    const right = document.createElement('span');
    right.className = 'file-right';
    const stats = document.createElement('span');
    stats.className = 'stats';
    const adds = document.createElement('span');
    adds.className = 'adds';
    adds.textContent = '+' + f.additions;
    const dels = document.createElement('span');
    dels.className = 'dels';
    dels.textContent = '−' + f.deletions;
    stats.appendChild(adds);
    stats.appendChild(document.createTextNode(' '));
    stats.appendChild(dels);
    right.appendChild(stats);
    summary.appendChild(right);
    details.appendChild(summary);
    if (f.binary) {
      const p = document.createElement('p');
      p.className = 'muted';
      p.textContent = 'Binary (or unsnapshotted oversized) file — contents not shown.';
      details.appendChild(p);
      return details;
    }
    const wrap = document.createElement('div');
    wrap.className = 'diff-wrap';
    const scroll = document.createElement('div');
    scroll.className = 'diff-scroll';
    const table = document.createElement('table');
    table.className = 'diff unified';
    const cell = (cls, text) => {
      const td = document.createElement('td');
      if (cls) td.className = cls;
      td.textContent = text;
      return td;
    };
    for (const hunk of f.hunks) {
      let highlighted = null;
      try { highlighted = callWasm('pd_highlight_hunk', f.new_path || f.old_path, JSON.stringify(hunk.lines)); }
      catch (e) { /* safe plain-text fallback below */ }
      const hr = document.createElement('tr');
      hr.className = 'hunk';
      hr.appendChild(cell('gutter', ''));
      hr.appendChild(cell('ln', ''));
      hr.appendChild(cell('ln', ''));
      hr.appendChild(cell('', hunk.header));
      table.appendChild(hr);
      for (let lineIndex = 0; lineIndex < hunk.lines.length; lineIndex++) {
        const line = hunk.lines[lineIndex];
        const kind = Object.keys(line)[0];
        const p = line[kind];
        const tr = document.createElement('tr');
        if (kind === 'Meta') {
          tr.className = 'meta-line';
          tr.appendChild(cell('gutter', ''));
          tr.appendChild(cell('ln', ''));
          tr.appendChild(cell('ln', ''));
          tr.appendChild(cell('', p.text));
        } else {
          tr.className = kind === 'Add' ? 'add' : (kind === 'Del' ? 'del' : 'ctx');
          const sign = kind === 'Add' ? '+' : (kind === 'Del' ? '-' : ' ');
          tr.appendChild(cell('gutter', ''));
          tr.appendChild(cell('ln', p.old !== undefined ? String(p.old) : ''));
          tr.appendChild(cell('ln', p.new !== undefined ? String(p.new) : ''));
          const code = cell('code', sign);
          const source = document.createElement('span');
          source.className = 'code-line';
          if (highlighted) source.innerHTML = highlighted[lineIndex];
          else source.textContent = p.text;
          code.appendChild(source);
          tr.appendChild(code);
        }
        table.appendChild(tr);
      }
    }
    scroll.appendChild(table);
    wrap.appendChild(scroll);
    details.appendChild(wrap);
    return details;
  }

  // ---- expand hunk context ----
  // Snapshots carry the full endpoint contents of every changed file, so the
  // page can reveal the unchanged lines a hunk gap hides. Gaps shorter than
  // COLLAPSE_MIN render immediately; every larger gap gets an expander row,
  // and a click reveals up to EXPAND_STEP lines adjacent to the diff via
  // pd_context_slice (the engine guarantees the region is identical at both
  // endpoints). Revealed rows are real commentable context rows with the same
  // anchors as build-time rows. Gap sizes come from the hunk headers plus the
  // endpoint line counts stamped on the file panel — no probing. Trailing
  // gaps reveal from the top (adjacent to the hunk above); every other gap
  // reveals from the bottom (adjacent to the hunk below). Browser-tested in
  // "short hunk gaps stay visible" and "hunk context expands".
  const COLLAPSE_MIN = 3;
  const EXPAND_STEP = 20;
  function hunkBounds(headerText) {
    const m = /@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/.exec(headerText);
    if (!m) return null;
    const a = Number(m[1]), b = m[2] === undefined ? 1 : Number(m[2]);
    const c = Number(m[3]), d = m[4] === undefined ? 1 : Number(m[4]);
    // A zero-count side names the line BEFORE the hunk (git convention).
    return {
      oldFirst: b === 0 ? a + 1 : a, oldLast: b === 0 ? a : a + b - 1,
      newFirst: d === 0 ? c + 1 : c, newLast: d === 0 ? c : c + d - 1,
    };
  }
  let expanderSeq = 0;
  function expanderLabel(tr) {
    const hidden = Number(tr.dataset.oldTo) - Number(tr.dataset.oldFrom) + 1;
    return hidden <= EXPAND_STEP
      ? 'Show all ' + hidden + ' hidden line' + (hidden === 1 ? '' : 's')
      : 'Show ' + EXPAND_STEP + ' of ' + hidden + ' hidden lines';
  }
  function expanderRow(gap) {
    const tr = document.createElement('tr');
    tr.className = 'expander';
    tr.dataset.expId = 'x' + (++expanderSeq);
    tr.dataset.oldFrom = gap.oldFrom;
    tr.dataset.oldTo = gap.oldTo;
    tr.dataset.newFrom = gap.newFrom;
    tr.dataset.newTo = gap.newTo;
    if (gap.trailing) tr.dataset.trailing = '1';
    for (const cls of ['gutter', 'ln', 'ln']) {
      const td = document.createElement('td');
      td.className = cls;
      tr.appendChild(td);
    }
    const td = document.createElement('td');
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'expander-btn';
    btn.textContent = expanderLabel(tr);
    td.appendChild(btn);
    tr.appendChild(td);
    return tr;
  }
  function gapContent(file, gap) {
    const count = gap.oldTo - gap.oldFrom + 1;
    if (count >= COLLAPSE_MIN) return expanderRow(gap);
    try {
      return contextFragment(file, gap.oldFrom, gap.newFrom, count);
    } catch (e) {
      // Keep the context reachable if eager expansion fails; clicking the
      // fallback expander reports the underlying error through the normal UI.
      return expanderRow(gap);
    }
  }
  function setupExpanders() {
    if (!SNAPSHOTS) return;
    document.querySelectorAll('#files-full details.file').forEach((file) => {
      const d = file.dataset;
      if (!d.oldPath || !d.newPath || d.oldLines === undefined || d.newLines === undefined) return;
      const table = file.querySelector('table.diff.unified');
      if (!table) return;
      let prevOld = 0;
      let prevNew = 0;
      let sawHunk = false;
      for (const row of Array.prototype.slice.call(table.rows)) {
        if (!row.classList.contains('hunk')) continue;
        const bounds = hunkBounds(row.cells[row.cells.length - 1].textContent);
        if (!bounds) continue;
        if (bounds.oldFirst > prevOld + 1) {
          const gap = {
            oldFrom: prevOld + 1, oldTo: bounds.oldFirst - 1,
            newFrom: prevNew + 1, newTo: bounds.newFirst - 1,
          };
          row.before(gapContent(file, gap));
        }
        prevOld = bounds.oldLast;
        prevNew = bounds.newLast;
        sawHunk = true;
      }
      if (sawHunk && Number(d.oldLines) > prevOld) {
        const gap = {
          oldFrom: prevOld + 1, oldTo: Number(d.oldLines),
          newFrom: prevNew + 1, newTo: Number(d.newLines),
          trailing: true,
        };
        (table.tBodies[0] || table).appendChild(gapContent(file, gap));
      }
    });
  }
  function contextRow(anchor, p, highlighted) {
    const tr = document.createElement('tr');
    tr.className = 'ctx commentable';
    tr.dataset.file = anchor;
    tr.dataset.side = 'New';
    tr.dataset.line = String(p.new);
    const gutter = document.createElement('td');
    gutter.className = 'gutter';
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'gutter-btn';
    btn.textContent = '+';
    btn.dataset.file = anchor;
    btn.dataset.side = 'New';
    btn.dataset.line = String(p.new);
    btn.setAttribute('aria-label', 'Add comment on New line ' + p.new);
    gutter.appendChild(btn);
    tr.appendChild(gutter);
    const oldLn = document.createElement('td');
    oldLn.className = 'ln';
    oldLn.textContent = String(p.old);
    tr.appendChild(oldLn);
    const newLn = document.createElement('td');
    newLn.className = 'ln';
    newLn.textContent = String(p.new);
    tr.appendChild(newLn);
    const code = document.createElement('td');
    code.className = 'code';
    code.appendChild(document.createTextNode(' '));
    const source = document.createElement('span');
    source.className = 'code-line';
    if (highlighted !== null) source.innerHTML = highlighted;
    else source.textContent = p.text;
    code.appendChild(source);
    tr.appendChild(code);
    return tr;
  }
  function contextFragment(file, oldStart, newStart, count) {
    const lines = callWasm('pd_context_slice', SNAPSHOTS, JSON.stringify({
      old_path: file.dataset.oldPath, new_path: file.dataset.newPath,
      old_start: oldStart, new_start: newStart, count: count,
    }));
    let highlighted = null;
    try {
      highlighted = callWasm('pd_highlight_lines', file.dataset.anchor,
        JSON.stringify(lines.map((line) => line.Ctx.text)));
    } catch (e) { /* safe plain-text fallback in contextRow */ }
    const frag = document.createDocumentFragment();
    for (let i = 0; i < lines.length; i++) {
      if (lines[i].Ctx) frag.appendChild(contextRow(
        file.dataset.anchor, lines[i].Ctx, highlighted ? highlighted[i] : null));
    }
    return frag;
  }
  // Split tables carry cloned proxy expanders; resolve back to the unified
  // original (the single source of gap state) by the stable id.
  function unifiedExpanderOf(btn) {
    const row = btn.closest('tr.expander');
    if (!row) return null;
    if (row.closest('table.diff.unified')) return row;
    const file = row.closest('details.file');
    if (!file) return null;
    return Array.prototype.find.call(
      file.querySelectorAll('table.diff.unified tr.expander'),
      (tr) => tr.dataset.expId === row.dataset.expId) || null;
  }
  document.addEventListener('click', (ev) => {
    const btn = ev.target.closest('.expander-btn');
    if (!btn) return;
    ev.preventDefault();
    ev.stopPropagation();
    const tr = unifiedExpanderOf(btn);
    if (!tr) return;
    const file = tr.closest('details.file');
    const oldFrom = Number(tr.dataset.oldFrom), oldTo = Number(tr.dataset.oldTo);
    const newFrom = Number(tr.dataset.newFrom), newTo = Number(tr.dataset.newTo);
    const take = Math.min(EXPAND_STEP, oldTo - oldFrom + 1);
    if (take <= 0) { tr.remove(); return; }
    const trailing = tr.dataset.trailing === '1';
    let frag;
    try {
      frag = contextFragment(file,
        trailing ? oldFrom : oldTo - take + 1,
        trailing ? newFrom : newTo - take + 1,
        take);
    } catch (e) {
      showError('Expand failed: ' + e.message);
      return;
    }
    if (trailing) {
      tr.before(frag);
      tr.dataset.oldFrom = oldFrom + take;
      tr.dataset.newFrom = newFrom + take;
    } else {
      tr.after(frag);
      tr.dataset.oldTo = oldTo - take;
      tr.dataset.newTo = newTo - take;
    }
    if (Number(tr.dataset.oldTo) < Number(tr.dataset.oldFrom)) tr.remove();
    else tr.querySelector('.expander-btn').textContent = expanderLabel(tr);
    // Rebuild this file's split (if built) so both views agree, then re-place
    // comments — an unanchored comment may now have its line on the page.
    const split = file.querySelector('table.diff.split');
    if (split) {
      split.remove();
      buildSplitsIn(file);
      syncSplitDisplay(file);
    }
    invalidateAnchorIndex();
    renderAll();
  });

  // ---- exports ----
  function copyText(text, btn) {
    const done = () => {
      if (!btn) { showToast('Copied'); return; }
      const old = btn.textContent;
      btn.textContent = 'Copied!';
      setTimeout(() => { btn.textContent = old; }, 1200);
    };
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(done, () => fallbackCopy(text, done));
    } else {
      fallbackCopy(text, done);
    }
  }
  function fallbackCopy(text, done) {
    const ta = document.createElement('textarea');
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    try { document.execCommand('copy'); } catch (e) { /* best effort */ }
    ta.remove();
    done();
  }

  function copyMd(btn) {
    copyText(callWasm('pd_export_markdown', JSON.stringify(doc)), btn);
  }

  document.getElementById('copy-md').addEventListener('click', (ev) => copyMd(ev.target));
  document.getElementById('copy-json').addEventListener('click', (ev) =>
    copyText(callWasm('pd_export_json', JSON.stringify(doc)), ev.target));
  // Import merges a copied/exported review back in — the pull half of the
  // copy-as push paths. The engine's pd_merge keeps it deterministic.
  document.getElementById('import-json').addEventListener('change', (ev) => {
    const file = ev.target.files && ev.target.files[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = () => {
      try {
        const before = doc.comments.length;
        saveDoc(callWasm('pd_merge', JSON.stringify(doc), String(reader.result)));
        // Report what landed and what did not anchor — comments outside this
        // rendering must be visible, not silently parked.
        const added = doc.comments.length - before;
        const unanchored = document.querySelectorAll('#unanchored .comment-card').length;
        showToast('Imported ' + added + ' comment' + (added === 1 ? '' : 's') +
          (unanchored ? ' — ' + unanchored + ' outside this diff, shown at the top' : ''));
      } catch (e) {
        showError('Import failed: ' + e.message);
      }
      ev.target.value = '';
    };
    reader.readAsText(file);
  });

  // ---- side-by-side (split) ----
  const SPLIT_MIN_EM = 90;

  function splitLnCell(no, kind) {
    const td = document.createElement('td');
    td.className = 'ln' + (kind ? ' ' + kind : '');
    td.textContent = no;
    return td;
  }
  function splitGutter(anchor, kind) {
    const td = document.createElement('td');
    td.className = 'gutter' + (kind ? ' ' + kind : '');
    if (anchor) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'gutter-btn';
      btn.textContent = '+';
      btn.dataset.file = anchor.file;
      btn.dataset.side = anchor.side;
      btn.dataset.line = anchor.line;
      btn.setAttribute('aria-label',
        'Add comment on ' + anchor.side + ' line ' + anchor.line);
      td.appendChild(btn);
    }
    return td;
  }
  function splitCodeCell(text, html, kind, anchor) {
    const td = document.createElement('td');
    td.className = 'code' + (kind ? ' ' + kind : '');
    if (anchor) {
      td.classList.add('commentable');
      td.dataset.file = anchor.file;
      td.dataset.side = anchor.side;
      td.dataset.line = anchor.line;
    }
    const source = document.createElement('span');
    source.className = 'code-line';
    if (html !== null) source.innerHTML = html;
    else source.textContent = text;
    td.appendChild(source);
    return td;
  }
  function splitEmpty(tr) {
    tr.appendChild(splitGutter(null, ''));
    const ln = document.createElement('td');
    ln.className = 'ln empty';
    const code = document.createElement('td');
    code.className = 'code empty';
    tr.appendChild(ln);
    tr.appendChild(code);
  }
  function unifiedCell(row, side) {
    // unified: gutter | old ln | new ln | code
    const isOld = side === 'Old';
    const lnIdx = isOld ? 1 : 2;
    const no = row.cells[lnIdx].textContent;
    const source = row.cells[3].querySelector('.code-line');
    const text = source ? source.textContent : row.cells[3].textContent.slice(1);
    const html = source ? source.innerHTML : null;
    const anchor = (row.classList.contains('commentable') && row.dataset.side === side)
      ? { file: row.dataset.file, side: side, line: row.dataset.line } : null;
    return { no: no, text: text, html: html, anchor: anchor };
  }

  function buildSplit(unified) {
    const split = document.createElement('table');
    split.className = 'diff split';
    const cols = document.createElement('colgroup');
    for (const cls of ['gutter-col', 'ln-col', '', 'gutter-col', 'ln-col', '']) {
      const col = document.createElement('col');
      if (cls) col.className = cls;
      cols.appendChild(col);
    }
    split.appendChild(cols);
    const thead = document.createElement('thead');
    const thr = document.createElement('tr');
    const thOld = document.createElement('th');
    thOld.colSpan = 3;
    thOld.textContent = 'Old';
    const thNew = document.createElement('th');
    thNew.colSpan = 3;
    thNew.textContent = 'New';
    thr.appendChild(thOld);
    thr.appendChild(thNew);
    thead.appendChild(thr);
    split.appendChild(thead);

    let dels = [], adds = [];
    function flush() {
      const n = Math.max(dels.length, adds.length);
      for (let i = 0; i < n; i++) {
        const tr = document.createElement('tr');
        if (dels[i]) {
          const d = unifiedCell(dels[i], 'Old');
          tr.appendChild(splitGutter(d.anchor, 'del'));
          tr.appendChild(splitLnCell(d.no, 'del'));
          tr.appendChild(splitCodeCell(d.text, d.html, 'del', d.anchor));
        } else { splitEmpty(tr); }
        if (adds[i]) {
          const a = unifiedCell(adds[i], 'New');
          tr.appendChild(splitGutter(a.anchor, 'add'));
          tr.appendChild(splitLnCell(a.no, 'add'));
          tr.appendChild(splitCodeCell(a.text, a.html, 'add', a.anchor));
        } else { splitEmpty(tr); }
        split.appendChild(tr);
      }
      dels = []; adds = [];
    }
    for (const row of unified.rows) {
      if (row.classList.contains('comment-row') || row.classList.contains('editor-row')) continue;
      if (row.parentElement && row.parentElement.tagName === 'THEAD') continue;
      if (row.classList.contains('expander')) {
        // Proxy: same id and label; clicks resolve back to the unified
        // original via unifiedExpanderOf, and expansion rebuilds this table.
        flush();
        const tr = document.createElement('tr');
        tr.className = 'expander';
        for (const k of Object.keys(row.dataset)) tr.dataset[k] = row.dataset[k];
        const td = document.createElement('td');
        td.colSpan = 6;
        const src = row.querySelector('.expander-btn');
        if (src) td.appendChild(src.cloneNode(true));
        tr.appendChild(td);
        split.appendChild(tr);
        continue;
      }
      if (row.classList.contains('hunk') || row.classList.contains('meta-line')) {
        flush();
        const tr = document.createElement('tr');
        tr.className = row.className;
        const td = document.createElement('td');
        td.colSpan = 6;
        td.textContent = row.cells[row.cells.length - 1].textContent;
        tr.appendChild(td);
        split.appendChild(tr);
      } else if (row.classList.contains('ctx')) {
        flush();
        const c = unifiedCell(row, 'New');
        const oldNo = row.cells[1].textContent;
        const tr = document.createElement('tr');
        tr.appendChild(splitGutter(null, ''));
        tr.appendChild(splitLnCell(oldNo, ''));
        tr.appendChild(splitCodeCell(c.text, c.html, '', null));
        tr.appendChild(splitGutter(c.anchor, ''));
        tr.appendChild(splitLnCell(c.no, ''));
        tr.appendChild(splitCodeCell(c.text, c.html, '', c.anchor));
        split.appendChild(tr);
      } else if (row.classList.contains('del')) {
        dels.push(row);
      } else if (row.classList.contains('add')) {
        adds.push(row);
      }
    }
    flush();
    return split;
  }

  function buildSplitsIn(root) {
    root.querySelectorAll('table.diff.unified').forEach((u) => {
      const scroll = u.parentElement;
      const wrap = scroll.classList.contains('diff-scroll') ? scroll.parentElement : scroll;
      if (!wrap.querySelector('table.diff.split')) {
        const split = buildSplit(u);
        if (scroll.classList.contains('diff-scroll')) scroll.appendChild(split);
        else wrap.appendChild(split);
      }
    });
    invalidateAnchorIndex();
  }
  // Which table each wrap shows. A wrap whose split is not built yet keeps
  // its unified table visible — panels are never empty; they swap when the
  // observer builds their split.
  function syncSplitDisplay(root) {
    const split = document.body.classList.contains('view-split');
    root.querySelectorAll('.diff-wrap').forEach((w) => {
      const u = w.querySelector('table.diff.unified');
      const s = w.querySelector('table.diff.split');
      if (u) u.style.display = split && s ? 'none' : '';
      if (s) s.style.display = split ? '' : 'none';
    });
  }
  function replaceCommentsIn(file) {
    const drafts = captureDrafts().filter((d) => d.anchor.file === file.dataset.anchor);
    file.querySelectorAll('.pd-editor').forEach((el) => closeEditor(el));
    file.querySelectorAll('tr.comment-row').forEach((el) => el.remove());
    for (const c of doc.comments) {
      if (c.file === file.dataset.anchor) placeComment(c);
    }
    restoreDrafts(drafts);
  }
  // Split tables double a file's diff DOM; building every file's at once is
  // the biggest single main-thread stall on large reviews. Panels near the
  // viewport build immediately; the observer builds the rest as they
  // approach, then re-places that file's comments and open drafts.
  let splitObserver = null;
  function ensureSplitBuilt() {
    document.querySelectorAll('table.diff.unified').forEach((u) => {
      if (!u.closest('details.file')) {
        const scroll = u.parentElement;
        buildSplitsIn(scroll.classList.contains('diff-scroll') ? scroll.parentElement : scroll);
      }
    });
    if (!splitObserver) {
      splitObserver = new IntersectionObserver((entries) => {
        for (const entry of entries) {
          if (!entry.isIntersecting) continue;
          splitObserver.unobserve(entry.target);
          if (!document.body.classList.contains('view-split')) continue;
          buildSplitsIn(entry.target);
          syncSplitDisplay(entry.target);
          replaceCommentsIn(entry.target);
        }
      }, { rootMargin: '1200px 0px' });
    }
    document.querySelectorAll('#files-full details.file, #files-range details.file').forEach((f) => {
      if (!f.querySelector('table.diff.split')) splitObserver.observe(f);
    });
  }
  function captureDrafts() {
    return Array.prototype.map.call(document.querySelectorAll('.pd-editor'), (el) => ({
      anchor: { file: el.dataset.file, side: el.dataset.side, line: el.dataset.line },
      editing: el.dataset.editing || null,
      text: el.querySelector('textarea').value,
    }));
  }
  function restoreDrafts(drafts) {
    for (const d of drafts) {
      const existing = d.editing ? doc.comments.find((c) => c.id === d.editing) : null;
      if (d.editing && !existing) continue;
      if (existing) openEditorFor(existing);
      else {
        const block = blockFor(d.anchor);
        if (block) openPreviewEditor(block, d.anchor, null);
        else {
          const row = rowFor(d.anchor);
          if (row) openTableEditor(row, d.anchor, null);
        }
      }
      const el = findEditor(d.anchor, d.editing);
      if (el) {
        el.querySelector('textarea').value = d.text;
        setDraft(d.anchor, d.editing, d.text);
      }
    }
  }
  function applyDiffView() {
    const split = document.body.classList.contains('view-split');
    if (split) ensureSplitBuilt();
    const drafts = captureDrafts();
    closeEditors();
    syncSplitDisplay(document);
    renderAll();
    restoreDrafts(drafts);
    updateCount();
  }

  const viewToggle = document.getElementById('view-toggle');
  function workspaceWidth() {
    // Measure the width the SPLIT layout would get. Unified mode caps
    // main#content at 1400px — below the 90em threshold at default fonts —
    // so measuring the current content width would disable the toggle
    // everywhere, forever. Split mode relaxes the cap to 1920px.
    return Math.min(document.documentElement.clientWidth || window.innerWidth, 1920);
  }
  function splitFits() {
    const fs = parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
    return workspaceWidth() >= SPLIT_MIN_EM * fs;
  }
  function refreshToggle() {
    if (!viewToggle) return;
    const fits = splitFits();
    viewToggle.disabled = !fits;
    viewToggle.title = fits
      ? 'Switch between unified and side-by-side diff'
      : 'Not enough horizontal space to render side-by-side';
    if (!fits && document.body.classList.contains('view-split')) {
      document.body.classList.remove('view-split');
      viewToggle.textContent = 'Side-by-side';
      viewToggle.setAttribute('aria-pressed', 'false');
      applyDiffView();
    }
  }
  if (viewToggle) {
    viewToggle.addEventListener('click', () => {
      const nowSplit = !document.body.classList.contains('view-split');
      document.body.classList.toggle('view-split', nowSplit);
      viewToggle.textContent = nowSplit ? 'Unified' : 'Side-by-side';
      viewToggle.setAttribute('aria-pressed', nowSplit ? 'true' : 'false');
      applyDiffView();
    });
    window.addEventListener('resize', refreshToggle);
    refreshToggle();
  }

  function applyFileWraps() {
    document.querySelectorAll('details.file').forEach((file) => {
      const nowrap = prefs.nowrap_files.indexOf(file.dataset.anchor) >= 0;
      file.classList.toggle('nowrap-lines', nowrap);
      const button = file.querySelector('.file-wrap-toggle');
      if (button) {
        button.textContent = nowrap ? 'Scroll' : 'Wrap';
        button.setAttribute('aria-pressed', nowrap ? 'false' : 'true');
      }
    });
  }
  document.addEventListener('click', (ev) => {
    const button = ev.target.closest('.file-wrap-toggle');
    if (!button) return;
    ev.preventDefault(); ev.stopPropagation();
    const file = button.closest('details.file');
    const set = new Set(prefs.nowrap_files);
    if (set.has(file.dataset.anchor)) set.delete(file.dataset.anchor);
    else set.add(file.dataset.anchor);
    prefs.nowrap_files = Array.from(set);
    savePrefs(); applyFileWraps();
  });

  // theme buttons
  function setTheme(t) {
    prefs.theme = t;
    savePrefs();
    applyTheme();
  }
  const ts = document.getElementById('theme-system');
  const tl = document.getElementById('theme-light');
  const td = document.getElementById('theme-dark');
  if (ts) ts.addEventListener('click', () => setTheme('system'));
  if (tl) tl.addEventListener('click', () => setTheme('light'));
  if (td) td.addEventListener('click', () => setTheme('dark'));

  // Expand / collapse all file panels. Collapse state is deliberate view
  // state (user data), so the bulk action writes prefs the same way the
  // per-file toggles do.
  function setAllFilesOpen(open) {
    const files = document.querySelectorAll('#files-full details.file');
    files.forEach((f) => { f.open = open; });
    prefs.collapsed_files = open ? [] : Array.prototype.map.call(files, (f) => f.dataset.anchor);
    savePrefs();
  }
  document.getElementById('expand-all').addEventListener('click', () => setAllFilesOpen(true));
  document.getElementById('collapse-all').addEventListener('click', () => setAllFilesOpen(false));


  // file scrollspy
  (function () {
    const panels = () => Array.prototype.slice.call(document.querySelectorAll('#files-full details.file'));
    let raf = 0;
    function update() {
      raf = 0;
      const chrome = document.getElementById('topnav');
      const top = (chrome ? chrome.offsetHeight : 0) + 8;
      let current = null;
      for (const p of panels()) {
        if (p.getBoundingClientRect().top <= top + 4) current = p;
      }
      renderCurrentFile(current);
      // The document-section tabs follow passive scrolling as well as
      // explicit anchor navigation. Other chrome links are not scroll-spy tabs.
      const links = document.querySelectorAll('#topnav a.section-link, #change-count');
      const sections = Array.prototype.map.call(links, (a) => {
        const id = a.getAttribute('href').slice(1);
        return { a: a, sec: document.getElementById(id) };
      }).filter((x) => x.sec);
      if (sections.length) {
        let cur = sections[0];
        for (const s of sections) {
          if (s.sec.getBoundingClientRect().top <= top + 4) cur = s;
        }
        const passiveId = current && cur.sec.id === 'diff' ? current.id : cur.sec.id;
        const locationId = explicitNavigationId || passiveId;
        const activeSectionId = locationId && locationId.indexOf('file-') === 0 ? 'diff' : locationId;
        for (const s of sections) {
          const active = s.sec.id === activeSectionId;
          s.a.classList.toggle('active', active);
          if (active) s.a.setAttribute('aria-current', 'location');
          else s.a.removeAttribute('aria-current');
        }
        if (locationId && location.hash !== '#' + locationId) history.replaceState(null, '', '#' + locationId);
      }
    }
    window.addEventListener('scroll', () => {
      if (!raf) raf = requestAnimationFrame(update);
    }, { passive: true });
    update();
  })();

  function renderCurrentFile(file) {
    const host = document.getElementById('current-file-context');
    if (!host) return;
    const id = file ? file.id : '';
    if (host.dataset.fileId === id) return;
    host.dataset.fileId = id;
    host.textContent = '';
    if (!file) return;
    const path = file.dataset.path || '';
    const pathHost = document.createElement('span');
    pathHost.className = 'context-path';
    const pieces = path.split('/');
    pieces.forEach((piece, index) => {
      const button = document.createElement('button');
      button.type = 'button';
      button.textContent = (index ? '/' : '') + piece;
      button.dataset.prefix = pieces.slice(0, index + 1).join('/');
      pathHost.appendChild(button);
    });
    host.appendChild(pathHost);
    const stats = document.createElement('span');
    stats.className = 'context-stats';
    stats.innerHTML = '<span class="adds">+' + file.dataset.adds + '</span> ' +
      '<span class="dels">−' + file.dataset.dels + '</span>';
    host.appendChild(stats);
    const comments = doc ? doc.comments.filter((c) => c.file === file.dataset.anchor).length : 0;
    if (comments) {
      const button = document.createElement('button');
      button.type = 'button'; button.className = 'ghost context-comments';
      button.textContent = comments + ' comment' + (comments === 1 ? '' : 's');
      button.addEventListener('click', () => file.querySelector('.file-comment-count').click());
      host.appendChild(button);
    }
    requestAnimationFrame(() => { pathHost.scrollLeft = pathHost.scrollWidth; });
  }
  document.getElementById('current-file-context').addEventListener('click', (ev) => {
    const crumb = ev.target.closest('.context-path button');
    if (!crumb) return;
    const prefix = crumb.dataset.prefix;
    const links = Array.prototype.filter.call(document.querySelectorAll('#filelist-full .fl-path a'),
      (a) => a.textContent.indexOf(prefix + '/') === 0 || a.textContent === prefix);
    if (!links.length) return;
    const row = links[0].closest('tr');
    row.scrollIntoView({ block: 'center' });
    row.setAttribute('tabindex', '-1'); row.focus({ preventScroll: true });
    for (const link of links) link.closest('tr').classList.add('arrival-flash');
    setTimeout(() => links.forEach((link) => link.closest('tr').classList.remove('arrival-flash')), 950);
    history.replaceState(null, '', '#files');
  });


  // ---- keyboard help + navigation ----
  function focusableWithin(root) {
    return Array.prototype.filter.call(root.querySelectorAll(
      'a[href], button:not([disabled]), input:not([disabled]), textarea:not([disabled]), ' +
      'select:not([disabled]), [tabindex]:not([tabindex="-1"])'),
    (el) => !el.hidden && !el.closest('[hidden]'));
  }
  function trapDialogTab(ev, root) {
    if (ev.key !== 'Tab') return false;
    const items = focusableWithin(root);
    if (!items.length) { ev.preventDefault(); return true; }
    const first = items[0], last = items[items.length - 1];
    if (ev.shiftKey && document.activeElement === first) { ev.preventDefault(); last.focus(); return true; }
    if (!ev.shiftKey && document.activeElement === last) { ev.preventDefault(); first.focus(); return true; }
    return false;
  }
  let helpFocusReturn = null;
  function openHelp() {
    const d = document.getElementById('help-dialog');
    helpFocusReturn = document.activeElement;
    d.hidden = false;
    document.getElementById('help-close').focus();
  }
  function closeHelp() {
    document.getElementById('help-dialog').hidden = true;
    if (helpFocusReturn && helpFocusReturn.focus) helpFocusReturn.focus();
  }
  document.getElementById('help-open').addEventListener('click', openHelp);
  document.getElementById('help-close').addEventListener('click', closeHelp);
  document.getElementById('help-dialog').addEventListener('click', (ev) => {
    if (ev.target.id === 'help-dialog') closeHelp();
  });

  function isTypingTarget(el) {
    if (!el) return false;
    const tag = el.tagName;
    return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || el.isContentEditable;
  }
  function filePanels() {
    return Array.prototype.slice.call(document.querySelectorAll('#files-full details.file'));
  }
  function goFile(delta) {
    const panels = filePanels();
    if (!panels.length) return;
    const chrome = document.getElementById('topnav');
    const top = (chrome ? chrome.offsetHeight : 0) + 8;
    let idx = 0;
    for (let i = 0; i < panels.length; i++) {
      if (panels[i].getBoundingClientRect().top <= top + 4) idx = i;
    }
    const next = Math.max(0, Math.min(panels.length - 1, idx + delta));
    panels[next].open = true;
    panels[next].scrollIntoView({ block: 'start' });
    panels[next].setAttribute('tabindex', '-1');
    panels[next].focus({ preventScroll: true });
    history.replaceState(null, '', '#' + panels[next].id);
  }
  function goComment(delta) {
    const cards = document.querySelectorAll(
      'tr.comment-row, .md-comments .comment-card, #unanchored .comment-card');
    if (!cards.length) { showToast('No comments'); return; }
    const list = Array.prototype.slice.call(cards);
    let idx = -1;
    const mid = window.innerHeight / 3;
    for (let i = 0; i < list.length; i++) {
      const r = list[i].getBoundingClientRect();
      if (r.top <= mid) idx = i;
    }
    const next = Math.max(0, Math.min(list.length - 1, idx + delta));
    // Land focus and the arrival cue on the card itself, not its table row.
    const target = list[next].querySelector('.comment-card') || list[next];
    list[next].scrollIntoView({ block: 'center' });
    target.setAttribute('tabindex', '-1');
    target.focus({ preventScroll: true });
    target.classList.add('arrival-flash');
    setTimeout(() => target.classList.remove('arrival-flash'), 950);
  }
  document.getElementById('comment-count').addEventListener('click', () => goComment(1));

  document.addEventListener('keydown', (ev) => {
    const help = document.getElementById('help-dialog');
    if (!help.hidden && trapDialogTab(ev, help)) return;
    if (ev.key === 'Escape') {
      if (!document.getElementById('help-dialog').hidden) { closeHelp(); return; }
      return;
    }
    if (isTypingTarget(ev.target)) return;
    if (ev.metaKey || ev.ctrlKey || ev.altKey) return;
    if (ev.key === '?') { ev.preventDefault(); openHelp(); return; }
    if (ev.key === 'j') { ev.preventDefault(); goFile(1); return; }
    if (ev.key === 'k') { ev.preventDefault(); goFile(-1); return; }
    if (ev.key === 'n') { ev.preventDefault(); goComment(1); return; }
    if (ev.key === 'p') { ev.preventDefault(); goComment(-1); return; }
  });

  // Close menus when clicking outside
  document.addEventListener('click', (ev) => {
    document.querySelectorAll('details.menu[open]').forEach((m) => {
      if (!m.contains(ev.target)) m.open = false;
    });
  });

  // Native <details> handles disclosure; this supplies expected menu keyboard
  // behavior for the menu items inside it.
  document.querySelectorAll('details.menu').forEach((menu) => {
    const summary = menu.querySelector('summary');
    const items = () => focusableWithin(menu.querySelector('.menu-panel'));
    summary.addEventListener('keydown', (ev) => {
      if (ev.key === 'ArrowDown' || ev.key === 'Enter' || ev.key === ' ') {
        ev.preventDefault();
        menu.open = true;
        requestAnimationFrame(() => {
          const list = items();
          if (list[0]) list[0].focus();
        });
      }
    });
    menu.addEventListener('keydown', (ev) => {
      const list = items();
      const index = list.indexOf(document.activeElement);
      if (ev.key === 'Escape') {
        ev.preventDefault(); menu.open = false; summary.focus(); return;
      }
      if (index < 0 || (ev.key !== 'ArrowDown' && ev.key !== 'ArrowUp')) return;
      ev.preventDefault();
      const delta = ev.key === 'ArrowDown' ? 1 : -1;
      list[(index + delta + list.length) % list.length].focus();
    });
  });

  // Restore drafts from prefs on load (short-lived recovery)
  function restorePersistedDrafts() {
    for (const k of Object.keys(prefs.drafts)) {
      const d = prefs.drafts[k];
      if (!d || !d.text) continue;
      // keys: file:side:line:new or file:side:line:edit:id
      const parts = k.split(':');
      if (parts.length < 4) continue;
      // file may contain colons rarely; last parts are side, line, kind...
      // Our draftKey uses file:side:line:new|edit:id — file is first segment until we hit side.
      // Safer: store structured; for now parse from known sides.
      const sideIdx = Math.max(parts.lastIndexOf('New'), parts.lastIndexOf('Old'));
      if (sideIdx < 1) continue;
      const file = parts.slice(0, sideIdx).join(':');
      const side = parts[sideIdx];
      const line = parts[sideIdx + 1];
      const kind = parts[sideIdx + 2];
      const editingId = kind === 'edit' ? parts[sideIdx + 3] : null;
      const anchor = { file: file, side: side, line: line };
      if (editingId) {
        const existing = doc.comments.find((c) => c.id === editingId);
        if (existing) {
          openEditorFor(existing);
          const el = findEditor(anchor, editingId);
          if (el) el.querySelector('textarea').value = d.text;
        }
      } else {
        const row = rowFor(anchor);
        const block = blockFor(anchor);
        if (block) openPreviewEditor(block, anchor, null);
        else if (row) openTableEditor(row, anchor, null);
        const el = findEditor(anchor, null);
        if (el) el.querySelector('textarea').value = d.text;
      }
    }
  }

  // Deep link: if URL has #file-N, ensure open
  if (location.hash && location.hash.indexOf('#file-') === 0) {
    const el = document.getElementById(location.hash.slice(1));
    if (el) el.open = true;
  }

  doc = initDoc();
  renderVerdict();
  document.querySelectorAll('details.file').forEach((file) => {
    file.open = prefs.collapsed_files.indexOf(file.dataset.anchor) < 0;
  });
  applyFileWraps();
  setupExpanders();
  document.querySelectorAll('.md-seg').forEach((seg) => {
    const file = seg.closest('details.file');
    const key = file ? file.dataset.anchor : '__description__';
    if (prefs.markdown_views[key] === 'source') {
      const source = seg.querySelector('[data-mdview="diff"], [data-mdview="raw"]');
      if (source) source.click();
    }
  });
  renderAll();
  renderChanges();
  restorePersistedDrafts();
})();
