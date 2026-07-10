'use strict';
// The PLAYER. packdiff splits the page in two — strict Rust for the ENGINE,
// vanilla JS for the PLAYER — a settled boundary, drawn by responsibility,
// not size (docs/ARCHITECTURE.md, "Web layer stance").
//
// This file owns presentation and browser state only: DOM assembly, event
// wiring, and view preferences (sidebar, wrap, theme, viewed files, drafts —
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

  const KEY = callWasm('pd_storage_key', META);
  const PREF_KEY = KEY + ':prefs';

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

  // ---- browser preferences (not part of the review document) ----
  const DEFAULT_PREFS = {
    schema_version: 1,
    viewed_files: [],
    sidebar_open: true,
    wrap_lines: false,
    theme: 'system',
    hide_viewed: false,
    drafts: {},
    comment_hint_seen: false,
  };
  let prefs = Object.assign({}, DEFAULT_PREFS, { viewed_files: [], drafts: {} });
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
        sidebar_open: parsed.sidebar_open !== false,
        wrap_lines: !!parsed.wrap_lines,
        theme: (parsed.theme === 'light' || parsed.theme === 'dark') ? parsed.theme : 'system',
        hide_viewed: !!parsed.hide_viewed,
        drafts: (parsed.drafts && typeof parsed.drafts === 'object') ? parsed.drafts : {},
        comment_hint_seen: !!parsed.comment_hint_seen,
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
  function applyWrap() {
    document.body.classList.toggle('wrap-lines', prefs.wrap_lines);
    const btn = document.getElementById('wrap-toggle');
    if (btn) {
      btn.setAttribute('aria-pressed', prefs.wrap_lines ? 'true' : 'false');
      btn.textContent = prefs.wrap_lines ? 'Do not wrap lines' : 'Wrap long lines';
    }
  }
  function applySidebarPref() {
    const wide = window.matchMedia('(min-width: 961px)').matches;
    if (wide) {
      document.body.classList.toggle('sidebar-collapsed', !prefs.sidebar_open);
      document.body.classList.remove('sidebar-open');
      const bd = document.getElementById('sidebar-backdrop');
      if (bd) bd.hidden = true;
    } else {
      document.body.classList.remove('sidebar-collapsed');
      // drawer closed by default on medium/mobile
    }
    const t = document.getElementById('sidebar-toggle');
    if (t) {
      const open = wide ? prefs.sidebar_open : document.body.classList.contains('sidebar-open');
      t.setAttribute('aria-expanded', open ? 'true' : 'false');
    }
  }

  loadPrefs();
  applyTheme();
  applyWrap();
  applySidebarPref();

  // ---- storage (review document) ----
  let doc = null;
  let memoryOnly = false;
  function showStorageWarning() {
    const el = document.getElementById('storage-warning');
    if (el) el.hidden = false;
    memoryOnly = true;
  }
  function initDoc() {
    let raw = null;
    try { raw = localStorage.getItem(KEY); } catch (e) { showStorageWarning(); }
    if (raw !== null) {
      try { return callWasm('pd_parse_document', raw); }
      catch (e) { console.warn('packdiff: stored document rejected (' + e.message + '); starting fresh'); }
    }
    return callWasm('pd_new_document', META);
  }
  function persist(next) {
    doc = next;
    if (!memoryOnly) {
      try { localStorage.setItem(KEY, JSON.stringify(doc)); }
      catch (e) { showStorageWarning(); }
    }
    updateCount();
    refreshSidebarMeta();
    if (summaryOpen()) renderSummary();
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
  function rowFor(c) {
    if (document.body.classList.contains('view-split')) {
      const cells = document.querySelectorAll(anchorSel('td.code.commentable', c));
      for (const cell of cells) {
        if (!cell.closest('[hidden]')) return cell.parentElement;
      }
      return cells[0] ? cells[0].parentElement : null;
    }
    // Prefer a row in a visible container (e.g. description Raw vs Preview).
    const rows = document.querySelectorAll(anchorSel('tr.commentable', c));
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

  // ---- counts / sidebar meta ----
  function updateCount() {
    const n = doc.comments.length;
    const label = n + ' comment' + (n === 1 ? '' : 's');
    const btn = document.getElementById('comment-count');
    if (btn) btn.textContent = 'Review · ' + label;
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
    });
  }

  function refreshSidebarMeta() {
    const counts = {};
    for (const c of doc.comments) {
      counts[c.file] = (counts[c.file] || 0) + 1;
    }
    document.querySelectorAll('.sidebar-row').forEach((row) => {
      const a = row.dataset.anchor;
      const n = counts[a] || 0;
      const el = row.querySelector('[data-role="comment-count"]');
      if (el) el.textContent = n > 0 ? String(n) : '';
      const viewed = prefs.viewed_files.indexOf(a) >= 0;
      row.classList.toggle('viewed', viewed);
    });
    // sync viewed checkboxes
    document.querySelectorAll('.file-viewed').forEach((cb) => {
      cb.checked = prefs.viewed_files.indexOf(cb.dataset.anchor) >= 0;
    });
    const total = document.querySelectorAll('#sidebar-list .sidebar-row').length;
    const viewed = prefs.viewed_files.filter((p) =>
      document.querySelector('.sidebar-row[data-anchor="' + cssAttr(p) + '"]')).length;
    const prog = document.getElementById('viewed-progress');
    if (prog) prog.textContent = viewed + ' of ' + total + ' files viewed';
    applyFileFilter();
  }

  function renderAll() {
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
    refreshSidebarMeta();
  }

  function commentCard(c) {
    const wrap = document.createElement('div');
    wrap.className = 'comment-card';
    wrap.dataset.commentId = c.id;
    const meta = document.createElement('div');
    meta.className = 'comment-meta';
    meta.textContent = c.file + ' · ' + c.side.toLowerCase() + ' line ' + c.line + ' · ' + c.updated_at;
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
    const del = document.createElement('button');
    del.type = 'button';
    del.textContent = 'Delete';
    del.addEventListener('click', () => {
      let next;
      try { next = callWasm('pd_delete_comment', JSON.stringify(doc), c.id); }
      catch (e) { showError('Delete failed: ' + e.message); return; }
      persist(next);
      removeCommentDom(c.id);
    });
    actions.appendChild(edit);
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
    refreshSidebarMeta();
    if (summaryOpen()) renderSummary();
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
      refreshSidebarMeta();
      if (summaryOpen()) renderSummary();
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
    // Warn if replacing a stored draft at a different editor for same anchor
    const existingDraft = getDraft(anchor, null);
    if (existingDraft && !findEditor(anchor, null)) {
      // reopening the same draft is fine; no prompt
    }
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

  // Gutter buttons open editors (not whole-line clicks)
  document.addEventListener('click', (ev) => {
    const gbtn = ev.target.closest('.gutter-btn');
    if (!gbtn) return;
    ev.preventDefault();
    ev.stopPropagation();
    const anchor = {
      file: gbtn.dataset.file,
      side: gbtn.dataset.side,
      line: gbtn.dataset.line,
    };
    if (!anchor.file) return;
    if (gbtn.closest('#files-range')) {
      showToast('The range view is read-only — comments attach to the full diff. Click “Show full diff” to comment.');
      return;
    }
    const block = gbtn.closest('.md-block-wrap')
      ? gbtn.closest('.md-block-wrap').querySelector('.md-block')
      : null;
    if (block) openNewComment(anchor, block);
    else {
      const row = gbtn.closest('tr');
      openNewComment(anchor, row);
    }
  });

  // Preview | Diff (markdown files) and Preview | Raw (description panel).
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
    renderAll();
    restoreDrafts(drafts);
    window.scrollTo(0, scrollY);
  });

  // File header controls
  document.addEventListener('click', (ev) => {
    const copy = ev.target.closest('button.copy-path');
    if (copy) {
      ev.preventDefault();
      copyText(copy.dataset.path, copy);
      return;
    }
    const nav = ev.target.closest('.file-prev, .file-next');
    if (nav) {
      ev.preventDefault();
      const id = nav.dataset.target;
      const el = document.getElementById(id);
      if (el) {
        el.open = true;
        el.scrollIntoView({ block: 'start' });
        history.replaceState(null, '', '#' + id);
      }
      return;
    }
  });
  document.addEventListener('change', (ev) => {
    const cb = ev.target.closest('.file-viewed');
    if (!cb) return;
    const anchor = cb.dataset.anchor;
    const set = new Set(prefs.viewed_files);
    if (cb.checked) set.add(anchor);
    else set.delete(anchor);
    prefs.viewed_files = Array.from(set);
    savePrefs();
    refreshSidebarMeta();
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
    if (ev.target.closest('input.commit-check')) return; // handled by change
    const row = ev.target.closest('tr.commit.selectable');
    if (!row || !SNAPSHOTS) return;
    const idx = Number(row.dataset.index);
    if (ev.shiftKey && rangeFrom !== null) {
      setRange(Math.min(rangeFrom, idx), Math.max(rangeTo, idx));
    } else if (rangeFrom === idx && rangeTo === idx) {
      setRange(null, null);
    } else {
      setRange(idx, idx);
    }
  });
  document.addEventListener('change', (ev) => {
    const cb = ev.target.closest('input.commit-check');
    if (!cb || !SNAPSHOTS) return;
    const idx = Number(cb.dataset.index);
    const checked = Array.prototype.map.call(
      document.querySelectorAll('input.commit-check:checked'),
      (el) => Number(el.dataset.index)
    ).sort((a, b) => a - b);
    if (checked.length === 0) setRange(null, null);
    else setRange(checked[0], checked[checked.length - 1]);
    // keep contiguous selection if user checked one in middle of gap
    if (checked.length && (rangeTo - rangeFrom + 1) !== checked.length) {
      // fill visual checks for contiguous range
      document.querySelectorAll('input.commit-check').forEach((el) => {
        const i = Number(el.dataset.index);
        el.checked = rangeFrom !== null && i >= rangeFrom && i <= rangeTo;
      });
    }
  });
  document.addEventListener('mousedown', (ev) => {
    if (ev.shiftKey && ev.target.closest('tr.commit.selectable')) ev.preventDefault();
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

  const FULL_COUNTS = {
    files: document.getElementById('nav-files').textContent,
    diff: document.getElementById('nav-diff').innerHTML,
  };
  function setNavCounts(filesText, diffHtml) {
    document.getElementById('nav-files').textContent = filesText;
    document.getElementById('nav-diff').innerHTML = diffHtml;
  }

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
      const cb = tr.querySelector('input.commit-check');
      if (cb) cb.checked = on;
    });
    if (rangeFrom === null) {
      if (bar) bar.hidden = true;
      rangedFiles.hidden = true;
      rangedFiles.textContent = '';
      if (rangedList) { rangedList.hidden = true; rangedList.textContent = ''; }
      fullFiles.style.display = '';
      if (fullList) fullList.style.display = '';
      setNavCounts(FULL_COUNTS.files, FULL_COUNTS.diff);
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
    setNavCounts(String(files.length),
      '<span class="adds">+' + adds + '</span> <span class="dels">−' + dels + '</span>');
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
      const hr = document.createElement('tr');
      hr.className = 'hunk';
      hr.appendChild(cell('gutter', ''));
      hr.appendChild(cell('ln', ''));
      hr.appendChild(cell('ln', ''));
      hr.appendChild(cell('', hunk.header));
      table.appendChild(hr);
      for (const line of hunk.lines) {
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
          tr.appendChild(cell('code', sign + p.text));
        }
        table.appendChild(tr);
      }
    }
    scroll.appendChild(table);
    wrap.appendChild(scroll);
    details.appendChild(wrap);
    return details;
  }

  // ---- exports ----
  function download(name, mime, text) {
    const a = document.createElement('a');
    a.href = URL.createObjectURL(new Blob([text], { type: mime }));
    a.download = name;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(a.href), 5000);
  }
  function stem() {
    return 'packdiff-comments-' + CONFIG.repo + '-' +
      CONFIG.base.sha.slice(0, 7) + '-' + CONFIG.head.sha.slice(0, 7);
  }
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

  function exportJson() {
    download(stem() + '.json', 'application/json',
      callWasm('pd_export_json', JSON.stringify(doc)));
  }
  function exportMd() {
    download(stem() + '.md', 'text/markdown',
      callWasm('pd_export_markdown', JSON.stringify(doc)));
  }
  function exportCsv() {
    download(stem() + '.csv', 'text/csv',
      callWasm('pd_export_csv', JSON.stringify(doc)));
  }
  function copyMd(btn) {
    copyText(callWasm('pd_export_markdown', JSON.stringify(doc)), btn);
  }

  document.getElementById('export-json').addEventListener('click', exportJson);
  document.getElementById('export-md').addEventListener('click', exportMd);
  document.getElementById('export-csv').addEventListener('click', exportCsv);
  document.getElementById('copy-md').addEventListener('click', (ev) => copyMd(ev.target));
  const summaryCopy = document.getElementById('summary-copy-md');
  if (summaryCopy) summaryCopy.addEventListener('click', (ev) => copyMd(ev.target));

  document.getElementById('import-json').addEventListener('change', (ev) => {
    const file = ev.target.files && ev.target.files[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = () => {
      try {
        saveDoc(callWasm('pd_merge', JSON.stringify(doc), String(reader.result)));
        showToast('Imported comments');
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
  function splitCodeCell(text, kind, anchor) {
    const td = document.createElement('td');
    td.className = 'code' + (kind ? ' ' + kind : '');
    if (anchor) {
      td.classList.add('commentable');
      td.dataset.file = anchor.file;
      td.dataset.side = anchor.side;
      td.dataset.line = anchor.line;
    }
    td.textContent = text;
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
    const text = row.cells[3].textContent.slice(1);
    const anchor = (row.classList.contains('commentable') && row.dataset.side === side)
      ? { file: row.dataset.file, side: side, line: row.dataset.line } : null;
    return { no: no, text: text, anchor: anchor };
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
          tr.appendChild(splitCodeCell(d.text, 'del', d.anchor));
        } else { splitEmpty(tr); }
        if (adds[i]) {
          const a = unifiedCell(adds[i], 'New');
          tr.appendChild(splitGutter(a.anchor, 'add'));
          tr.appendChild(splitLnCell(a.no, 'add'));
          tr.appendChild(splitCodeCell(a.text, 'add', a.anchor));
        } else { splitEmpty(tr); }
        split.appendChild(tr);
      }
      dels = []; adds = [];
    }
    for (const row of unified.rows) {
      if (row.classList.contains('comment-row') || row.classList.contains('editor-row')) continue;
      if (row.parentElement && row.parentElement.tagName === 'THEAD') continue;
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
        tr.appendChild(splitCodeCell(c.text, '', null));
        tr.appendChild(splitGutter(c.anchor, ''));
        tr.appendChild(splitLnCell(c.no, ''));
        tr.appendChild(splitCodeCell(c.text, '', c.anchor));
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

  function ensureSplitBuilt() {
    document.querySelectorAll('table.diff.unified').forEach((u) => {
      const scroll = u.parentElement;
      const wrap = scroll.classList.contains('diff-scroll') ? scroll.parentElement : scroll;
      if (!wrap.querySelector('table.diff.split')) {
        const split = buildSplit(u);
        if (scroll.classList.contains('diff-scroll')) scroll.appendChild(split);
        else wrap.appendChild(split);
      }
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
    document.querySelectorAll('.diff-wrap').forEach((w) => {
      const u = w.querySelector('table.diff.unified');
      const s = w.querySelector('table.diff.split');
      if (u) u.style.display = split ? 'none' : '';
      if (s) s.style.display = split ? '' : 'none';
    });
    renderAll();
    restoreDrafts(drafts);
    updateCount();
  }

  const viewToggle = document.getElementById('view-toggle');
  function workspaceWidth() {
    const main = document.getElementById('content');
    return main ? main.clientWidth : window.innerWidth;
  }
  function splitFits() {
    const fs = parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
    return workspaceWidth() >= SPLIT_MIN_EM * fs;
  }
  function refreshToggle() {
    if (!viewToggle) return;
    const fits = splitFits();
    viewToggle.hidden = !fits;
    viewToggle.disabled = !fits;
    viewToggle.title = fits
      ? 'Switch between unified and side-by-side diff'
      : 'Workspace too narrow for side-by-side';
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
    window.addEventListener('resize', () => {
      refreshToggle();
      applySidebarPref();
    });
    refreshToggle();
  }

  // wrap toggle
  const wrapToggle = document.getElementById('wrap-toggle');
  if (wrapToggle) {
    wrapToggle.addEventListener('click', () => {
      prefs.wrap_lines = !prefs.wrap_lines;
      savePrefs();
      applyWrap();
    });
  }

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

  // ---- sidebar ----
  function toggleSidebar() {
    const wide = window.matchMedia('(min-width: 961px)').matches;
    if (wide) {
      prefs.sidebar_open = !prefs.sidebar_open;
      savePrefs();
      applySidebarPref();
    } else {
      const open = !document.body.classList.contains('sidebar-open');
      document.body.classList.toggle('sidebar-open', open);
      const bd = document.getElementById('sidebar-backdrop');
      if (bd) bd.hidden = !open;
      const t = document.getElementById('sidebar-toggle');
      if (t) t.setAttribute('aria-expanded', open ? 'true' : 'false');
      if (open) {
        const close = document.getElementById('sidebar-close');
        if (close) close.focus();
      } else {
        const t2 = document.getElementById('sidebar-toggle');
        if (t2) t2.focus();
      }
    }
  }
  function closeSidebarDrawer() {
    document.body.classList.remove('sidebar-open');
    const bd = document.getElementById('sidebar-backdrop');
    if (bd) bd.hidden = true;
    const t = document.getElementById('sidebar-toggle');
    if (t) {
      t.setAttribute('aria-expanded', 'false');
      t.focus();
    }
  }
  document.getElementById('sidebar-toggle').addEventListener('click', toggleSidebar);
  const sidebarClose = document.getElementById('sidebar-close');
  if (sidebarClose) sidebarClose.addEventListener('click', closeSidebarDrawer);
  const sidebarBd = document.getElementById('sidebar-backdrop');
  if (sidebarBd) sidebarBd.addEventListener('click', closeSidebarDrawer);

  document.getElementById('sidebar-list').addEventListener('click', (ev) => {
    const row = ev.target.closest('.sidebar-row');
    if (!row) return;
    const id = 'file-' + row.dataset.fileIndex;
    const el = document.getElementById(id);
    if (el) {
      el.open = true;
      el.scrollIntoView({ block: 'start' });
      history.replaceState(null, '', '#' + id);
    }
    if (!window.matchMedia('(min-width: 961px)').matches) closeSidebarDrawer();
  });

  // file search + filters
  let activeFilter = 'all';
  const fileSearch = document.getElementById('file-search');
  function applyFileFilter() {
    const q = (fileSearch && fileSearch.value || '').trim().toLowerCase();
    let visible = 0;
    document.querySelectorAll('#sidebar-list .sidebar-row').forEach((row) => {
      const path = (row.dataset.path || '').toLowerCase();
      const status = row.dataset.status;
      const anchor = row.dataset.anchor;
      const comments = doc
        ? doc.comments.some((c) => c.file === anchor)
        : false;
      const viewed = prefs.viewed_files.indexOf(anchor) >= 0;
      let ok = !q || path.indexOf(q) >= 0;
      if (ok && activeFilter === 'M') ok = status === 'M';
      else if (ok && activeFilter === 'A') ok = status === 'A';
      else if (ok && activeFilter === 'D') ok = status === 'D';
      else if (ok && activeFilter === 'R') ok = status === 'R';
      else if (ok && activeFilter === 'comments') ok = comments;
      else if (ok && activeFilter === 'unviewed') ok = !viewed;
      if (ok && prefs.hide_viewed && viewed && activeFilter !== 'unviewed') ok = false;
      row.hidden = !ok;
      if (ok) visible++;
    });
    let empty = document.querySelector('#sidebar-list .sidebar-empty');
    if (visible === 0) {
      if (!empty) {
        empty = document.createElement('div');
        empty.className = 'sidebar-empty';
        document.getElementById('sidebar-list').appendChild(empty);
      }
      empty.textContent = 'No files match.';
      empty.hidden = false;
    } else if (empty) {
      empty.hidden = true;
    }
  }
  if (fileSearch) fileSearch.addEventListener('input', applyFileFilter);
  document.querySelectorAll('.sidebar-filters button').forEach((btn) => {
    btn.addEventListener('click', () => {
      activeFilter = btn.dataset.filter;
      document.querySelectorAll('.sidebar-filters button').forEach((b) => {
        const on = b === btn;
        b.setAttribute('aria-pressed', on ? 'true' : 'false');
      });
      applyFileFilter();
    });
  });
  const hideViewed = document.getElementById('hide-viewed');
  if (hideViewed) {
    hideViewed.addEventListener('click', () => {
      prefs.hide_viewed = !prefs.hide_viewed;
      savePrefs();
      hideViewed.setAttribute('aria-pressed', prefs.hide_viewed ? 'true' : 'false');
      applyFileFilter();
    });
    hideViewed.setAttribute('aria-pressed', prefs.hide_viewed ? 'true' : 'false');
  }
  const nextUnviewed = document.getElementById('next-unviewed');
  if (nextUnviewed) {
    nextUnviewed.addEventListener('click', () => {
      const rows = document.querySelectorAll('#sidebar-list .sidebar-row');
      for (const row of rows) {
        if (row.hidden) continue;
        if (prefs.viewed_files.indexOf(row.dataset.anchor) < 0) {
          row.click();
          return;
        }
      }
      showToast('All visible files are viewed');
    });
  }

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
      document.querySelectorAll('.sidebar-row').forEach((row) => {
        const id = 'file-' + row.dataset.fileIndex;
        row.classList.toggle('active', current && current.id === id);
      });
      // chrome section links
      const links = document.querySelectorAll('#topnav a.chrome-link');
      const sections = Array.prototype.map.call(links, (a) => {
        const id = a.getAttribute('href').slice(1);
        return { a: a, sec: document.getElementById(id) };
      }).filter((x) => x.sec);
      if (sections.length) {
        let cur = sections[0];
        for (const s of sections) {
          if (s.sec.getBoundingClientRect().top <= top + 4) cur = s;
        }
        for (const s of sections) s.a.classList.toggle('active', s === cur);
      }
    }
    window.addEventListener('scroll', () => {
      if (!raf) raf = requestAnimationFrame(update);
    }, { passive: true });
    update();
  })();

  // ---- review summary drawer ----
  let summaryFocusReturn = null;
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
    const first = items[0];
    const last = items[items.length - 1];
    if (ev.shiftKey && document.activeElement === first) {
      ev.preventDefault(); last.focus(); return true;
    }
    if (!ev.shiftKey && document.activeElement === last) {
      ev.preventDefault(); first.focus(); return true;
    }
    return false;
  }
  function summaryOpen() {
    const d = document.getElementById('summary-drawer');
    return d && !d.hidden && d.classList.contains('open');
  }
  function openSummary() {
    const drawer = document.getElementById('summary-drawer');
    const bd = document.getElementById('summary-backdrop');
    if (!drawer) return;
    summaryFocusReturn = document.activeElement;
    drawer.hidden = false;
    bd.hidden = false;
    requestAnimationFrame(() => drawer.classList.add('open'));
    renderSummary();
    const close = document.getElementById('summary-close');
    if (close) close.focus();
  }
  function closeSummary() {
    const drawer = document.getElementById('summary-drawer');
    const bd = document.getElementById('summary-backdrop');
    if (!drawer) return;
    drawer.classList.remove('open');
    drawer.hidden = true;
    bd.hidden = true;
    if (summaryFocusReturn) summaryFocusReturn.focus();
  }
  function renderSummary() {
    const body = document.getElementById('summary-body');
    if (!body) return;
    body.textContent = '';
    if (!doc.comments.length) {
      const p = document.createElement('p');
      p.className = 'summary-empty';
      p.textContent = 'No comments yet. Use the + gutter on any line to add one.';
      body.appendChild(p);
      return;
    }
    // group by file, preserve model order within
    const groups = new Map();
    for (const c of doc.comments) {
      if (!groups.has(c.file)) groups.set(c.file, []);
      groups.get(c.file).push(c);
    }
    // unanchored first if any
    const unanchored = [];
    const anchoredGroups = new Map();
    for (const [file, list] of groups) {
      const a = [], u = [];
      for (const c of list) {
        if (rowFor(c) || blockFor(c)) a.push(c);
        else u.push(c);
      }
      if (a.length) anchoredGroups.set(file, a);
      for (const c of u) unanchored.push(c);
    }
    if (unanchored.length) {
      const g = document.createElement('div');
      g.className = 'summary-group';
      const h = document.createElement('h3');
      h.textContent = 'Unanchored';
      g.appendChild(h);
      for (const c of unanchored) g.appendChild(summaryItem(c, false));
      body.appendChild(g);
    }
    for (const [file, list] of anchoredGroups) {
      const g = document.createElement('div');
      g.className = 'summary-group';
      const h = document.createElement('h3');
      h.textContent = file;
      g.appendChild(h);
      for (const c of list) g.appendChild(summaryItem(c, true));
      body.appendChild(g);
    }
  }
  function summaryItem(c, linkable) {
    const item = document.createElement('div');
    item.className = 'summary-item';
    const meta = document.createElement('div');
    meta.className = 'comment-meta';
    if (linkable) {
      const a = document.createElement('a');
      a.href = '#';
      a.textContent = c.side.toLowerCase() + ' line ' + c.line;
      a.addEventListener('click', (ev) => {
        ev.preventDefault();
        closeSummary();
        const row = rowFor(c);
        const block = blockFor(c);
        const target = row || block;
        if (target) {
          const panel = target.closest('details.file');
          if (panel) panel.open = true;
          target.scrollIntoView({ block: 'center' });
        }
      });
      meta.appendChild(a);
      meta.appendChild(document.createTextNode(' · ' + c.updated_at));
    } else {
      meta.textContent = c.file + ' · ' + c.side.toLowerCase() + ' line ' + c.line +
        ' · ' + c.updated_at + ' · not in this rendering';
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
    edit.addEventListener('click', () => {
      closeSummary();
      openEditorFor(c);
    });
    const del = document.createElement('button');
    del.type = 'button';
    del.textContent = 'Delete';
    del.addEventListener('click', () => {
      let next;
      try { next = callWasm('pd_delete_comment', JSON.stringify(doc), c.id); }
      catch (e) { showError('Delete failed: ' + e.message); return; }
      persist(next);
      removeCommentDom(c.id);
      renderSummary();
    });
    actions.appendChild(edit);
    actions.appendChild(del);
    item.appendChild(meta);
    item.appendChild(body);
    item.appendChild(actions);
    return item;
  }
  document.getElementById('comment-count').addEventListener('click', openSummary);
  document.getElementById('summary-close').addEventListener('click', closeSummary);
  document.getElementById('summary-backdrop').addEventListener('click', closeSummary);

  // ---- keyboard help + navigation ----
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
    list[next].scrollIntoView({ block: 'center' });
  }

  document.addEventListener('keydown', (ev) => {
    const help = document.getElementById('help-dialog');
    const summary = document.getElementById('summary-drawer');
    if (!help.hidden && trapDialogTab(ev, help)) return;
    if (summaryOpen() && trapDialogTab(ev, summary)) return;
    if (ev.key === 'Escape') {
      if (!document.getElementById('help-dialog').hidden) { closeHelp(); return; }
      if (summaryOpen()) { closeSummary(); return; }
      if (document.body.classList.contains('sidebar-open')) { closeSidebarDrawer(); return; }
      return;
    }
    if (isTypingTarget(ev.target)) return;
    if (ev.metaKey || ev.ctrlKey || ev.altKey) return;
    if (ev.key === '?') { ev.preventDefault(); openHelp(); return; }
    if (ev.key === 'j') { ev.preventDefault(); goFile(1); return; }
    if (ev.key === 'k') { ev.preventDefault(); goFile(-1); return; }
    if (ev.key === 'n') { ev.preventDefault(); goComment(1); return; }
    if (ev.key === 'p') { ev.preventDefault(); goComment(-1); return; }
    if (ev.key === 'f') {
      ev.preventDefault();
      if (!window.matchMedia('(min-width: 961px)').matches) {
        document.body.classList.add('sidebar-open');
        const bd = document.getElementById('sidebar-backdrop');
        if (bd) bd.hidden = false;
      } else if (!prefs.sidebar_open) {
        prefs.sidebar_open = true;
        savePrefs();
        applySidebarPref();
      }
      if (fileSearch) fileSearch.focus();
      return;
    }
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
  renderAll();
  // Don't auto-reopen every draft on load — only if single draft (less surprise)
  const draftKeys = Object.keys(prefs.drafts);
  if (draftKeys.length === 1) restorePersistedDrafts();
})();
