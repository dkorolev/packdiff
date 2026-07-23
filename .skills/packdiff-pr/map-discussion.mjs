#!/usr/bin/env node
// map-discussion.mjs — pure transform from a GitHub PR discussion bundle to
// the two offline discussion artifacts the packdiff-pr skill ships beside the
// generated page:
//
//   review.json    a schema v3 ReviewDocument for Actions → Import JSON —
//                  every line-anchored review thread becomes real packdiff
//                  comments, merged by the CRDT like any other import
//   discussion.md  the whole discussion — reviews, conversation, threads —
//                  for agents and humans; machine-shaped, lossy about nothing
//
// Usage: node map-discussion.mjs <bundle.json> <review.json> <discussion.md>
// Prints one single-key JSON summary to stdout (packdiff machine-mode style):
//   { "Mapped": { "threads": …, "comments": …, "fallback_anchored": …,
//                 "reviews": …, "conversation": …, "verdict": … } }
//
// The bundle is assembled by the skill (see SKILL.md step 6):
//   {
//     "pr":            { number, title, url, state, reviewDecision,
//                        repo, base: {name, sha}, head: {name, sha} },
//     "threads":       [ GraphQL reviewThreads nodes: isResolved, isOutdated,
//                        path, line, originalLine, diffSide,
//                        comments: { nodes: [ { databaseId, url, body,
//                          createdAt, lastEditedAt, author { login } } ] } ],
//     "reviews":       [ REST pulls/N/reviews items ],
//     "conversation":  [ REST issues/N/comments items ]
//   }
//
// Mapping decisions (deliberate, mirrored in SKILL.md):
// - Comment id is `github-rc-<databaseId>` — stable, so re-running the skill
//   and re-importing upserts instead of duplicating.
// - Registers are stamped { seq: 1, actor: "github:<login>" }. Ties between
//   two imports of the same comment resolve by timestamp, so a body edited on
//   GitHub wins on re-import; any local edit (seq >= 2) beats both, so a
//   re-import never clobbers local work.
// - Thread anchor maps directly: path → file, diffSide RIGHT → New / LEFT →
//   Old, line → line. An outdated thread (line null) falls back to
//   originalLine and is flagged in the comment text; the page shows comments
//   whose anchor left the diff in its "outside this diff" section.
// - isResolved → resolved_at. GitHub does not expose when a thread was
//   resolved, so the thread's last activity timestamp stands in — resolved_at
//   is display metadata; presence is what makes the state.
// - reviewDecision → the document verdict (APPROVED → Approved,
//   CHANGES_REQUESTED → ChangesRequired), stamped at the newest review that
//   carries the matching state. Anything else leaves the verdict absent.
// - Review summaries and conversation comments have no diff anchor, so they
//   travel in discussion.md only.

import { readFileSync, writeFileSync } from 'node:fs';

const [bundlePath, jsonPath, mdPath] = process.argv.slice(2);
if (!bundlePath || !jsonPath || !mdPath) {
  process.stderr.write('usage: map-discussion.mjs <bundle.json> <review.json> <discussion.md>\n');
  process.exit(2);
}

const bundle = JSON.parse(readFileSync(bundlePath, 'utf8'));
const pr = bundle.pr;
const threads = bundle.threads ?? [];
const reviews = bundle.reviews ?? [];
const conversation = bundle.conversation ?? [];

const login = (author) => (author && (author.login ?? author)) || 'ghost';
const nl = (s) => String(s ?? '').replace(/\r\n/g, '\n');
const commentNodes = (t) => t.comments?.nodes ?? t.comments ?? [];

// ---- review.json ----

const comments = [];
let fallbackAnchored = 0;
for (const t of threads) {
  const line = t.line ?? t.originalLine;
  if (t.line == null && t.originalLine != null) fallbackAnchored += 1;
  if (line == null) continue; // no anchor at all; discussion.md still has it
  const side = t.diffSide === 'LEFT' ? 'Old' : 'New';
  const nodes = commentNodes(t);
  const lastAt = nodes.map((c) => c.lastEditedAt ?? c.createdAt).sort().at(-1);
  for (const c of nodes) {
    const who = login(c.author);
    const flags = [
      t.isOutdated ? 'outdated thread — anchored to its original line' : '',
      c.url ? `[on GitHub](${c.url})` : '',
    ].filter(Boolean).join(' · ');
    comments.push({
      id: `github-rc-${c.databaseId}`,
      file: t.path,
      side,
      line,
      text: `**@${who}** · ${c.createdAt}${flags ? ' · ' + flags : ''}\n\n${nl(c.body)}`,
      created_at: c.createdAt,
      updated_at: c.lastEditedAt ?? c.createdAt,
      ...(t.isResolved ? { resolved_at: lastAt } : {}),
      version: { seq: 1, actor: `github:${who}` },
      resolution_version: { seq: 1, actor: `github:${who}` },
    });
  }
}

const decision = pr.reviewDecision;
let verdict, verdictVersion;
if (decision === 'APPROVED' || decision === 'CHANGES_REQUESTED') {
  const wanted = decision === 'APPROVED' ? 'APPROVED' : 'CHANGES_REQUESTED';
  const match = reviews.filter((r) => r.state === wanted && r.submitted_at)
    .sort((a, b) => a.submitted_at.localeCompare(b.submitted_at)).at(-1);
  if (match) {
    verdict = decision === 'APPROVED'
      ? { Approved: { at: match.submitted_at } }
      : { ChangesRequired: { at: match.submitted_at } };
    verdictVersion = { seq: 1, actor: `github:${login(match.user)}` };
  }
}

const doc = {
  schema_version: 3,
  tool: 'packdiff',
  repo: pr.repo,
  base: pr.base,
  head: pr.head,
  clock: 1,
  comments,
  ...(verdict ? { verdict, verdict_version: verdictVersion } : {}),
};
writeFileSync(jsonPath, JSON.stringify(doc, null, 2) + '\n');

// ---- discussion.md ----

const indent = (s) => nl(s).split('\n').map((l) => '  ' + l).join('\n');
const resolvedCount = threads.filter((t) => t.isResolved).length;
const outdatedCount = threads.filter((t) => t.isOutdated).length;

const md = [];
md.push(`# PR discussion — ${pr.repo}#${pr.number}: ${pr.title}`, '');
md.push(`- URL: ${pr.url}`);
md.push(`- State: ${pr.state}${decision ? ` · review decision: ${decision}` : ''}`);
md.push(`- ${threads.length} review thread(s) (${resolvedCount} resolved, ` +
  `${outdatedCount} outdated) · ${reviews.length} review(s) · ` +
  `${conversation.length} conversation comment(s)`, '');

if (reviews.length) {
  md.push('## Reviews', '');
  for (const r of reviews) {
    md.push(`- **@${login(r.user)}** — ${r.state} · ${r.submitted_at ?? 'not submitted'}`);
    if (nl(r.body).trim()) md.push('', indent(r.body), '');
  }
  md.push('');
}

if (conversation.length) {
  md.push('## Conversation', '');
  for (const c of conversation) {
    md.push(`- **@${login(c.user)}** · ${c.created_at}`);
    md.push('', indent(c.body), '');
  }
}

if (threads.length) {
  md.push('## Review threads', '');
  const byFile = new Map();
  for (const t of threads) {
    const list = byFile.get(t.path);
    if (list) list.push(t); else byFile.set(t.path, [t]);
  }
  for (const [file, list] of [...byFile.entries()].sort((a, b) => a[0].localeCompare(b[0]))) {
    md.push(`### ${file}`, '');
    for (const t of list) {
      const side = t.diffSide === 'LEFT' ? 'old' : 'new';
      const where = t.line ?? t.originalLine;
      const flags = [t.isResolved ? 'resolved' : '', t.isOutdated ? 'outdated' : '']
        .filter(Boolean).join(', ');
      md.push(`- **L${where ?? '?'} (${side})**${flags ? ` — ${flags}` : ''}`);
      for (const c of commentNodes(t)) {
        md.push(`  - **@${login(c.author)}** · ${c.createdAt}`);
        md.push('', indent(indent(c.body)), '');
      }
    }
  }
}
writeFileSync(mdPath, md.join('\n').replace(/\n{3,}/g, '\n\n').trimEnd() + '\n');

process.stdout.write(JSON.stringify({
  Mapped: {
    threads: threads.length,
    comments: comments.length,
    fallback_anchored: fallbackAnchored,
    reviews: reviews.length,
    conversation: conversation.length,
    verdict: verdict ? Object.keys(verdict)[0] : null,
  },
}, null, 2) + '\n');
