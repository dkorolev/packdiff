//! The review document: the comments a reviewer leaves on a diff, and every
//! operation that may change them.
//!
//! This is the state that lives in the browser's localStorage. The page's
//! JavaScript never edits it directly — it calls into the WASM build of this
//! crate for every mutation, so upsert/delete/merge/ordering semantics are
//! defined exactly once, here, and are identical in tests, in the CLI, and in
//! the browser.
//!
//! Purity contract: the model has no clock and no entropy. Comment `id`s,
//! actor ids, and RFC 3339 timestamps are supplied by the caller; the model
//! validates and orders them. The one counter the model does own — the
//! document's Lamport [`ReviewDocument::clock`] — is deterministic.
//!
//! # The merge is a CRDT
//!
//! Local-first, offline-first, CRDT-first: two people review one diff
//! offline and exchange JSON through the page's Actions menu, so
//! [`ReviewDocument::merge`] must be a join — commutative, associative,
//! idempotent — and it must not trust anyone's wall clock. Three ingredients:
//!
//! - **A Lamport clock per document.** Every write stamps the mutated
//!   register with a [`Version`] `{ seq, actor }` where `seq` is the
//!   document clock plus one; merge takes the elementwise maximum of clocks.
//!   A write made after observing version `n` therefore always carries
//!   `seq > n`: causal order without wall clocks.
//! - **Two registers per comment, not one.** Content (`text` + the anchor)
//!   and resolution (`resolved_at`) carry independent versions, because
//!   resolving is the one mutation a *different* person routinely makes to
//!   someone else's comment — "A polishes wording" concurrent with "B
//!   resolves" must keep both. Splitting further would buy nothing: nothing
//!   else changes after creation.
//! - **Tombstones.** `delete` writes a versioned [`Tombstone`] instead of
//!   forgetting; a merged-in copy of a deleted comment stays dead only when
//!   the delete causally observed that text (`tombstone seq > content seq` —
//!   an observer necessarily ticked past what it saw). A concurrent or
//!   later content write revives the comment — between "lose written text"
//!   and "see a deleted comment return", packdiff keeps the text, because
//!   text is the irreplaceable thing.
//!   There is deliberately no tombstone GC: a tombstone is tiny and a store
//!   is scoped to one diff, so the unbounded case does not exist inside a
//!   review's lifetime.
//!
//! The verdict is one more register, whose value may be *absent* — so
//! clearing a verdict finally merges instead of silently un-clearing on
//! every import.
//!
//! Rejected shapes, so they stay rejected: CRDT dependencies (the engine is
//! dependency-free and the wasm ships inlined in every page — size is a
//! budget); vector clocks (they buy concurrency *detection*, which no UI
//! surface renders, at per-actor space — Lamport plus an actor tiebreak
//! resolves identically in O(1)); hybrid logical clocks (nothing reads
//! logical time as time — `updated_at` already serves the humans);
//! character-wise text merge (a review comment is short prose; a human
//! expects "my rewrite" to survive whole, not interleaved).
//!
//! # Compatibility
//!
//! v1/v2 documents parse with every version defaulted to `{ seq: 0, actor:
//! "" }` and `clock: 0`. So that two upgraded stores still merge exactly as
//! v2 did (newer `updated_at` wins), register comparison keys are
//! `(seq, timestamp, actor)` — the timestamp component is inert once
//! `seq >= 1` and reproduces v2's outcome at `seq == 0`. One transient
//! asymmetry is accepted: a post-upgrade write beats any pre-upgrade write
//! for the same id regardless of wall clock — causality cannot be
//! reconstructed from v2 data, and pretending otherwise would reintroduce
//! the clock race permanently to smooth a one-time upgrade edge.

use serde::{Deserialize, Serialize};

use crate::{ModelError, RefInfo, SCHEMA_VERSION, TOOL};

/// Which side of the diff a comment anchors to. Serialized as the bare
/// `CamelCase` variant name (`"Old"` / `"New"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Side {
  /// Pre-image (a deleted line): `line` is an old-file line number.
  Old,
  /// Post-image (an added or context line): `line` is a new-file line number.
  New,
}

/// A register's write version: the document's Lamport clock at the write,
/// plus the writing actor as the deterministic tiebreaker. `{ seq: 0,
/// actor: "" }` marks data upgraded from a pre-CRDT (v1/v2) document; every
/// real write carries `seq >= 1`.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Version {
  /// Lamport sequence: `document clock + 1` at write time.
  #[serde(default)]
  pub seq: u64,
  /// Caller-supplied opaque writer id (one per browser profile, generated
  /// like comment ids — the model stays entropy-free). Byte order breaks
  /// `seq` ties between actors.
  #[serde(default)]
  pub actor: String,
}

impl Version {
  /// True for the upgraded-from-v2 placeholder; such versions are omitted
  /// from JSON so an untouched upgraded document stays byte-stable.
  pub fn is_zero(&self) -> bool {
    self.seq == 0 && self.actor.is_empty()
  }
}

/// The total order registers merge by: Lamport `seq` first; then, only
/// meaningful while both sides are upgraded v2 data at `seq == 0`, the
/// register's own wall-clock timestamp (which is exactly what v2 compared);
/// then the actor. Equal keys mean the same write — or hand-edited stores,
/// which the callers settle with one further deterministic comparison.
fn version_key<'a>(v: &'a Version, timestamp: &'a str) -> (u64, &'a str, &'a str) {
  (v.seq, timestamp, &v.actor)
}

/// The reviewer's verdict on the whole change, GitHub-style. Encoded as a
/// single-key union — `{ "Approved": { "at": "…" } }` — with no
/// discriminator field. The timestamp is what humans and exports read; the
/// merge is decided by the document's `verdict_version` register.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Verdict {
  /// The change is approved as it stands.
  Approved {
    /// When the verdict was given, RFC 3339 UTC, caller-supplied.
    at: String,
  },
  /// The change needs work before it can land.
  ChangesRequired {
    /// When the verdict was given, RFC 3339 UTC, caller-supplied.
    at: String,
  },
}

impl Verdict {
  /// When the verdict was given — display metadata, and the `seq == 0`
  /// comparison component that keeps upgraded-v2 merges faithful.
  pub fn at(&self) -> &str {
    match self {
      Verdict::Approved { at } | Verdict::ChangesRequired { at } => at,
    }
  }

  /// Human label for exports: `approved` / `changes required`.
  pub fn label(&self) -> &'static str {
    match self {
      Verdict::Approved { .. } => "approved",
      Verdict::ChangesRequired { .. } => "changes required",
    }
  }

  fn validate(&self) -> Result<(), ModelError> {
    if self.at().trim().is_empty() {
      return Err(ModelError::InvalidVerdict("at must be non-empty".to_string()));
    }
    Ok(())
  }
}

/// One review comment, anchored to a diff line.
///
/// Anchor identity is `(file, side, line)` where `file` is the post-image
/// path ([`crate::diff::FileDiff::anchor_path`]). Comment identity is `id` —
/// anchors may collide (several comments on one line), ids may not.
///
/// A comment is two independently-versioned LWW registers: **content**
/// (`text` and the anchor, under [`Comment::version`]) and **resolution**
/// (`resolved_at`, under [`Comment::resolution_version`]) — see the module
/// doc for why the split is exactly here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Comment {
  /// Caller-supplied unique id — the comment's identity for upsert, delete,
  /// and import merging.
  pub id: String,
  /// Post-image path of the file the comment anchors to.
  pub file: String,
  /// Which side of the diff `line` counts in.
  pub side: Side,
  /// 1-based line number on `side`.
  pub line: u32,
  /// The comment body, verbatim; may span multiple lines.
  pub text: String,
  /// Creation time, RFC 3339 UTC, caller-supplied; part of the stable sort
  /// order so threads on one line read chronologically.
  pub created_at: String,
  /// Last-edit time, RFC 3339 UTC, caller-supplied — what humans and
  /// exports read, and the `seq == 0` merge component for upgraded v2 data.
  pub updated_at: String,
  /// When the comment was resolved, RFC 3339 UTC, caller-supplied.
  /// Present = resolved; absent (and omitted from JSON) = open. Reopening
  /// removes it. Absent from v1 documents, which parse as all-open.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub resolved_at: Option<String>,
  /// The content register's write version. On upsert the caller supplies
  /// only `actor`; the model stamps `seq`. Zero (and omitted) for upgraded
  /// v1/v2 data.
  #[serde(default, skip_serializing_if = "Version::is_zero")]
  pub version: Version,
  /// The resolution register's write version, independent of `version` so
  /// a concurrent edit and resolve both survive a merge.
  #[serde(default, skip_serializing_if = "Version::is_zero")]
  pub resolution_version: Version,
}

impl Comment {
  /// Validation applied on every entry into the document.
  pub fn validate(&self) -> Result<(), ModelError> {
    fn need(cond: bool, msg: &str) -> Result<(), ModelError> {
      if cond {
        Ok(())
      } else {
        Err(ModelError::InvalidComment(msg.to_string()))
      }
    }
    need(!self.id.trim().is_empty(), "id must be non-empty")?;
    need(!self.file.trim().is_empty(), "file must be non-empty")?;
    need(self.line >= 1, "line must be >= 1")?;
    need(!self.text.trim().is_empty(), "text must be non-empty")?;
    need(!self.created_at.trim().is_empty(), "created_at must be non-empty")?;
    need(!self.updated_at.trim().is_empty(), "updated_at must be non-empty")?;
    if let Some(at) = &self.resolved_at {
      need(!at.trim().is_empty(), "resolved_at, when present, must be non-empty")?;
    }
    Ok(())
  }

  /// Deterministic document order: by file, then line, then side (old
  /// before new), then creation time, then id as the final tiebreaker.
  fn sort_key(&self) -> (&str, u32, Side, &str, &str) {
    (&self.file, self.line, self.side, &self.created_at, &self.id)
  }

  /// The content register: the anchor, the text, and the creation time.
  /// `updated_at` is deliberately outside it — it is display metadata that
  /// rides whichever copy wins (and the page bumps it on resolve, which
  /// must not tick the content register), and `resolved_at` is the other
  /// register. Two comments with equal content need no register decision.
  fn same_content(&self, other: &Comment) -> bool {
    self.file == other.file
      && self.side == other.side
      && self.line == other.line
      && self.text == other.text
      && self.created_at == other.created_at
  }

  /// True when `other`'s content register should replace `self`'s in a
  /// merge. Equal keys with differing content (hand-edited stores at the
  /// same version) are settled by comparing the content itself, so the join
  /// stays commutative even for input the model never produced.
  fn content_loses_to(&self, other: &Comment) -> bool {
    let (a, b) = (version_key(&self.version, &self.updated_at), version_key(&other.version, &other.updated_at));
    b > a || (b == a && !self.same_content(other) && content_order(other) > content_order(self))
  }

  /// As [`Comment::content_loses_to`] for the resolution register; the
  /// deterministic settle-equal-keys comparison is the resolution value.
  fn resolution_loses_to(&self, other: &Comment) -> bool {
    let ts = |c: &Comment| c.resolved_at.clone().unwrap_or_default();
    let (sts, ots) = (ts(self), ts(other));
    let (a, b) = (version_key(&self.resolution_version, &sts), version_key(&other.resolution_version, &ots));
    b > a || (b == a && ots > sts)
  }
}

/// A stable projection of the content register for the equal-version,
/// different-content edge — any total order works; this one reads.
fn content_order(c: &Comment) -> (&str, &str, u32, &str, &str) {
  (&c.text, &c.file, c.line, &c.created_at, &c.updated_at)
}

/// The versioned fact that a comment id was deleted. Kept apart from the
/// living comments so exports and the page keep iterating `comments`
/// untouched; carried by the canonical JSON (deletions must travel through
/// export/import, or they would resurrect through the exchange path).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tombstone {
  /// The deleted comment's id.
  pub id: String,
  /// The delete's write version. Tombstones postdate the CRDT, so this is
  /// never zero.
  pub deleted: Version,
  /// The dead comment's joined register state. Discarding it would lose
  /// information a later merge may need (a resolution write reaching one
  /// replica before the delete and another after would diverge them —
  /// associativity demands the registers survive the kill). `Some` exactly
  /// while the tombstone dominates; a revived comment's tombstone drops its
  /// shadow, since a content register that once dominated the delete
  /// dominates it forever.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub shadow: Option<Comment>,
}

/// The mutable review state for one `(repo, base_sha, head_sha)` diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewDocument {
  /// Schema generation this document was written with; readers reject
  /// documents newer than they understand ([`SCHEMA_VERSION`]). Every write
  /// modernizes the document to the current generation.
  pub schema_version: u32,
  /// Producing tool identifier (`"packdiff"`); distinguishes exported files
  /// from look-alike JSON of other origins.
  pub tool: String,
  /// Repository directory name the review belongs to.
  pub repo: String,
  /// The reviewed diff's base ref, pinned to its SHA.
  pub base: RefInfo,
  /// The reviewed diff's head ref, pinned to its SHA.
  pub head: RefInfo,
  /// The comments, always in the canonical order ([`ReviewDocument::sort`]).
  pub comments: Vec<Comment>,
  /// The reviewer's verdict on the whole change; `None` (and omitted from
  /// JSON) while the review is in progress — and in v1 documents, which
  /// parse as verdict-less.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub verdict: Option<Verdict>,
  /// The verdict register's write version. Zero for upgraded v1/v2
  /// documents; stamped on every [`ReviewDocument::set_verdict`], including
  /// the clearing one — which is what makes clearing merge.
  #[serde(default, skip_serializing_if = "Version::is_zero")]
  pub verdict_version: Version,
  /// The document's Lamport clock: the highest `seq` it has ever written or
  /// merged in. Zero (and omitted) only before the first post-upgrade write.
  #[serde(default, skip_serializing_if = "u64_is_zero")]
  pub clock: u64,
  /// Deleted comment ids with their delete versions, ordered by id. Never
  /// garbage-collected — see the module doc for why that is a decision and
  /// not an oversight.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub tombstones: Vec<Tombstone>,
}

fn u64_is_zero(n: &u64) -> bool {
  *n == 0
}

impl ReviewDocument {
  pub fn new(repo: String, base: RefInfo, head: RefInfo) -> Self {
    Self {
      schema_version: SCHEMA_VERSION,
      tool: TOOL.to_string(),
      repo,
      base,
      head,
      comments: Vec::new(),
      verdict: None,
      verdict_version: Version::default(),
      clock: 0,
      tombstones: Vec::new(),
    }
  }

  /// Parse and validate a document. Rejects documents from a newer schema
  /// and any unknown field (strict deserialization); re-validates and
  /// re-sorts every comment so untrusted input (an imported file, a
  /// hand-edited store) is normalized on the way in. v1/v2 documents parse
  /// with zero versions and a zero clock — the upgraded-in-place form the
  /// module doc describes.
  pub fn parse(json: &str) -> Result<Self, ModelError> {
    let mut doc: ReviewDocument = serde_json::from_str(json).map_err(|e| ModelError::Json(e.to_string()))?;
    if doc.schema_version > SCHEMA_VERSION {
      return Err(ModelError::UnsupportedSchema { found: doc.schema_version, supported: SCHEMA_VERSION });
    }
    for c in &doc.comments {
      c.validate()?;
      // A hand-raised version must not outrun the clock, or the next write
      // could fail to dominate it. Absorbing it into the clock is cheaper
      // and kinder than rejecting the document.
      doc.clock = doc.clock.max(c.version.seq).max(c.resolution_version.seq);
    }
    if let Some(v) = &doc.verdict {
      v.validate()?;
    }
    doc.clock = doc.clock.max(doc.verdict_version.seq);
    for t in &doc.tombstones {
      if t.id.trim().is_empty() {
        return Err(ModelError::InvalidComment("tombstone id must be non-empty".to_string()));
      }
      if let Some(shadow) = &t.shadow {
        shadow.validate()?;
      }
      let shadow_seq = t.shadow.as_ref().map_or(0, |s| s.version.seq.max(s.resolution_version.seq));
      doc.clock = doc.clock.max(t.deleted.seq).max(shadow_seq);
    }
    // A hand-edited store may hold a live comment its own tombstone
    // dominates; settling here keeps the invariant a boundary property.
    doc.settle();
    doc.sort();
    Ok(doc)
  }

  /// Canonical serialized form (pretty, trailing newline) — what exports
  /// and the localStorage round-trip use.
  pub fn to_json_pretty(&self) -> String {
    let mut s = serde_json::to_string_pretty(self).expect("document serializes: no non-string keys, no fallible types");
    s.push('\n');
    s
  }

  /// One write's version stamp: advances the clock and modernizes the
  /// document's schema claim (an upgraded v2 store becomes a v3 store on
  /// its first write, never silently before).
  fn tick(&mut self, actor: &str) -> Version {
    self.clock += 1;
    self.schema_version = SCHEMA_VERSION;
    Version { seq: self.clock, actor: actor.to_string() }
  }

  /// Insert a new comment or replace the one with the same `id`.
  ///
  /// The caller supplies `comment.version.actor` (the writer's id) and
  /// leaves the sequence to the model: each register that actually changed
  /// is stamped with the next clock value, so an untouched register keeps
  /// its version — a resolve does not steal a concurrent edit's win, and
  /// vice versa. Upserting a tombstoned id revives it: the fresh stamp
  /// dominates the tombstone by construction, and the tombstone's shadow
  /// (the dead registers) is what the write is diffed against.
  pub fn upsert(&mut self, comment: Comment) -> Result<(), ModelError> {
    comment.validate()?;
    let actor = comment.version.actor.clone();
    let (prior, reviving) = match self.comments.iter().position(|c| c.id == comment.id) {
      Some(i) => (Some(self.comments.remove(i)), false),
      // An upsert against a tombstoned id is an assertion of liveness (the
      // page's undo-of-delete replays the dead bytes verbatim), so content
      // ticks even when nothing changed — the fresh stamp is what outranks
      // the tombstone.
      None => (self.tombstones.iter_mut().find(|t| t.id == comment.id).and_then(|t| t.shadow.take()), true),
    };
    let mut next = comment;
    match prior {
      Some(prior) => {
        next.version = if prior.same_content(&next) && !reviving { prior.version.clone() } else { self.tick(&actor) };
        next.resolution_version =
          if prior.resolved_at == next.resolved_at { prior.resolution_version.clone() } else { self.tick(&actor) };
      }
      None => {
        next.version = self.tick(&actor);
        next.resolution_version = next.version.clone();
      }
    }
    self.comments.push(next);
    self.settle();
    self.sort();
    Ok(())
  }

  /// Set, replace, or (with `None`) clear the review verdict — every case
  /// is a stamped write, so every case merges.
  pub fn set_verdict_by(&mut self, verdict: Option<Verdict>, actor: &str) -> Result<(), ModelError> {
    if let Some(v) = &verdict {
      v.validate()?;
    }
    self.verdict_version = self.tick(actor);
    self.verdict = verdict;
    Ok(())
  }

  /// [`ReviewDocument::set_verdict_by`] with an anonymous actor — kept for
  /// callers that predate actors (native tests, tooling); merges still
  /// converge, the tiebreak is just less discriminating.
  pub fn set_verdict(&mut self, verdict: Option<Verdict>) -> Result<(), ModelError> {
    self.set_verdict_by(verdict, "")
  }

  /// Remove a comment by id, leaving a versioned tombstone so the deletion
  /// is a fact that merges instead of an absence that resurrects. The dead
  /// comment's registers move into the tombstone's shadow — deletion hides,
  /// it never destroys information. Returns whether anything was removed;
  /// deleting an unknown id is a no-op (there is nothing observed to
  /// delete, so no tombstone either).
  pub fn delete_by(&mut self, id: &str, actor: &str) -> bool {
    let Some(i) = self.comments.iter().position(|c| c.id == id) else {
      return false;
    };
    let dead = self.comments.remove(i);
    let stamp = self.tick(actor);
    match self.tombstones.iter_mut().find(|t| t.id == id) {
      Some(t) => {
        t.deleted = stamp;
        match &mut t.shadow {
          Some(shadow) => join_registers(shadow, &dead),
          None => t.shadow = Some(dead),
        }
      }
      None => self.tombstones.push(Tombstone { id: id.to_string(), deleted: stamp, shadow: Some(dead) }),
    }
    self.sort();
    true
  }

  /// [`ReviewDocument::delete_by`] with an anonymous actor.
  pub fn delete(&mut self, id: &str) -> bool {
    self.delete_by(id, "")
  }

  /// Merge another document into this one (the import path). This is a
  /// state-CRDT join: commutative, associative, idempotent — property-tested
  /// below, exchange order cannot change where replicas converge.
  ///
  /// Per register, the greater `(seq, timestamp, actor)` key wins (see
  /// [`version_key`]); content and resolution decide independently, so a
  /// concurrent edit and resolve both land. Tombstones union by id, greater
  /// version kept; a tombstone suppresses a comment only when its `seq`
  /// strictly exceeds the comment's content `seq` — i.e. only a delete that
  /// causally observed that text kills it; a concurrent edit revives.
  /// Clocks join by max. Metadata (repo/refs) of `self` is
  /// kept — importing does not re-target a store.
  pub fn merge(&mut self, incoming: &ReviewDocument) {
    self.clock = self.clock.max(incoming.clock);
    if incoming.schema_version > self.schema_version {
      self.schema_version = incoming.schema_version;
    }
    // Join every incoming register copy — live and shadowed alike — into
    // our state for that id, wherever ours lives.
    for inc in incoming.comments.iter().chain(incoming.tombstones.iter().filter_map(|t| t.shadow.as_ref())) {
      self.join_comment(inc);
    }
    // Join the tombstone facts (greater delete version kept).
    for inc in &incoming.tombstones {
      match self.tombstones.iter_mut().find(|t| t.id == inc.id) {
        Some(t) => {
          if inc.deleted > t.deleted {
            t.deleted = inc.deleted.clone();
          }
        }
        None => self.tombstones.push(Tombstone { id: inc.id.clone(), deleted: inc.deleted.clone(), shadow: None }),
      }
    }
    self.settle();
    // The verdict register joins like any other; on equal keys with
    // differing values (hand-edited stores) the serialized value decides,
    // keeping the join commutative.
    let ts = |v: &Option<Verdict>| v.as_ref().map(|v| v.at().to_string()).unwrap_or_default();
    let (sts, its) = (ts(&self.verdict), ts(&incoming.verdict));
    let (a, b) = (version_key(&self.verdict_version, &sts), version_key(&incoming.verdict_version, &its));
    let value = |v: &Option<Verdict>| serde_json::to_string(v).expect("verdict serializes");
    if b > a || (b == a && value(&incoming.verdict) > value(&self.verdict)) {
      self.verdict = incoming.verdict.clone();
      self.verdict_version = incoming.verdict_version.clone();
    }
    self.sort();
  }

  /// Join one incoming register copy into wherever our state for that id
  /// lives — the live comment, a tombstone's shadow, or (id never seen)
  /// the live list, with [`ReviewDocument::settle`] left to rule on
  /// liveness.
  fn join_comment(&mut self, inc: &Comment) {
    if let Some(existing) = self.comments.iter_mut().find(|c| c.id == inc.id) {
      join_registers(existing, inc);
    } else if let Some(shadow) = self.tombstones.iter_mut().find(|t| t.id == inc.id).and_then(|t| t.shadow.as_mut()) {
      join_registers(shadow, inc);
    } else {
      self.comments.push(inc.clone());
    }
  }

  /// Enforce the liveness invariant: for every tombstoned id, the register
  /// state sits in exactly one place. Liveness compares Lamport `seq`
  /// alone: a delete that OBSERVED a content write necessarily ticked past
  /// it, so `deleted.seq > content.seq` is exactly "the deleter saw this
  /// text" — kill (state moves into the shadow); equal seq is provably
  /// concurrent — the comment lives, keeping the text no matter whose
  /// actor id sorts higher, and any shadow folds back into it.
  fn settle(&mut self) {
    for ti in 0..self.tombstones.len() {
      match self.comments.iter().position(|c| c.id == self.tombstones[ti].id) {
        Some(ci) => {
          if self.tombstones[ti].deleted.seq > self.comments[ci].version.seq {
            let dead = self.comments.remove(ci);
            match &mut self.tombstones[ti].shadow {
              Some(shadow) => join_registers(shadow, &dead),
              None => self.tombstones[ti].shadow = Some(dead),
            }
          } else if let Some(shadow) = self.tombstones[ti].shadow.take() {
            join_registers(&mut self.comments[ci], &shadow);
          }
        }
        // No live comment: a shadow whose content write reached (or passed)
        // the delete revives — the join may have grown the shadow past the
        // tombstone, and liveness is a projection, not a ratchet.
        None => {
          let revives =
            self.tombstones[ti].shadow.as_ref().is_some_and(|s| s.version.seq >= self.tombstones[ti].deleted.seq);
          if revives {
            let shadow = self.tombstones[ti].shadow.take().expect("checked Some above");
            self.comments.push(shadow);
          }
        }
      }
    }
  }

  /// Restore the canonical deterministic order (comments and tombstones).
  pub fn sort(&mut self) {
    self.comments.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
    self.tombstones.sort_by(|a, b| a.id.cmp(&b.id));
  }
}

/// Join `inc` into `existing` register-wise: content and resolution decide
/// independently, so a concurrent edit and resolve both land.
fn join_registers(existing: &mut Comment, inc: &Comment) {
  if existing.content_loses_to(inc) {
    let keep_resolution = (existing.resolved_at.clone(), existing.resolution_version.clone());
    *existing = inc.clone();
    existing.resolved_at = keep_resolution.0;
    existing.resolution_version = keep_resolution.1;
  }
  if existing.resolution_loses_to(inc) {
    existing.resolved_at = inc.resolved_at.clone();
    existing.resolution_version = inc.resolution_version.clone();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn refinfo(name: &str, sha_char: char) -> RefInfo {
    RefInfo { name: name.into(), sha: std::iter::repeat(sha_char).take(40).collect() }
  }

  fn doc() -> ReviewDocument {
    ReviewDocument::new("repo".into(), refinfo("main", 'a'), refinfo("feat", 'b'))
  }

  fn comment(id: &str, file: &str, line: u32, updated: &str) -> Comment {
    Comment {
      id: id.into(),
      file: file.into(),
      side: Side::New,
      line,
      text: format!("note {id}"),
      created_at: "2026-07-03T10:00:00Z".into(),
      updated_at: updated.into(),
      resolved_at: None,
      version: Version::default(),
      resolution_version: Version::default(),
    }
  }

  fn actor_comment(id: &str, actor: &str, text: &str) -> Comment {
    let mut c = comment(id, "a.rs", 1, "2026-07-03T10:00:00Z");
    c.text = text.into();
    c.version.actor = actor.into();
    c
  }

  fn verdict_approved(at: &str) -> Verdict {
    Verdict::Approved { at: at.into() }
  }

  #[test]
  fn side_serializes_as_camelcase_variant_name() {
    assert_eq!(serde_json::to_value(Side::Old).unwrap(), serde_json::json!("Old"));
    assert_eq!(serde_json::to_value(Side::New).unwrap(), serde_json::json!("New"));
  }

  #[test]
  fn unknown_fields_are_rejected() {
    let mut v = serde_json::to_value(comment("c1", "a.rs", 5, "t")).unwrap();
    v.as_object_mut().unwrap().insert("sneaky".into(), serde_json::json!(true));
    assert!(serde_json::from_value::<Comment>(v).is_err());
  }

  #[test]
  fn upsert_inserts_then_replaces() {
    let mut d = doc();
    d.upsert(comment("c1", "a.rs", 5, "2026-07-03T10:00:00Z")).unwrap();
    assert_eq!(d.comments[0].version.seq, 1, "the first write is clock 1");
    let mut edited = comment("c1", "a.rs", 5, "2026-07-03T11:00:00Z");
    edited.text = "edited".into();
    d.upsert(edited).unwrap();
    assert_eq!(d.comments.len(), 1);
    assert_eq!(d.comments[0].text, "edited");
    assert_eq!(d.comments[0].version.seq, 2, "the edit advanced the content register");
    assert_eq!(d.comments[0].resolution_version.seq, 1, "the untouched resolution register kept its version");
  }

  #[test]
  fn upsert_rejects_invalid() {
    let mut d = doc();
    let mut bad = comment("c1", "a.rs", 5, "2026-07-03T10:00:00Z");
    bad.text = "   ".into();
    assert!(matches!(d.upsert(bad), Err(ModelError::InvalidComment(_))));
    let mut bad = comment("c2", "a.rs", 5, "2026-07-03T10:00:00Z");
    bad.line = 0;
    assert!(d.upsert(bad).is_err());
    assert!(d.comments.is_empty());
    assert_eq!(d.clock, 0, "rejected writes do not advance the clock");
  }

  #[test]
  fn delete_by_id_leaves_a_tombstone() {
    let mut d = doc();
    d.upsert(comment("c1", "a.rs", 5, "2026-07-03T10:00:00Z")).unwrap();
    assert!(d.delete("c1"));
    assert!(!d.delete("c1"), "the second delete has nothing to observe");
    assert!(d.comments.is_empty());
    assert_eq!(d.tombstones.len(), 1, "one tombstone, not one per attempt");
    assert_eq!(d.tombstones[0].id, "c1");
    assert_eq!(d.tombstones[0].deleted.seq, 2, "the delete is a stamped write");
    assert!(!d.delete("never-existed"), "deleting the unknown neither errs nor tombstones");
    assert_eq!(d.tombstones.len(), 1);
  }

  #[test]
  fn ordering_is_file_line_side_created_id() {
    let mut d = doc();
    d.upsert(comment("z", "b.rs", 1, "2026-07-03T10:00:00Z")).unwrap();
    d.upsert(comment("y", "a.rs", 9, "2026-07-03T10:00:00Z")).unwrap();
    d.upsert(comment("x", "a.rs", 2, "2026-07-03T10:00:00Z")).unwrap();
    let mut old_side = comment("w", "a.rs", 2, "2026-07-03T10:00:00Z");
    old_side.side = Side::Old;
    d.upsert(old_side).unwrap();
    let order: Vec<&str> = d.comments.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(order, vec!["w", "x", "y", "z"]); // old before new on the same line
  }

  #[test]
  fn merge_unions_and_causally_newer_wins() {
    let mut ours = doc();
    ours.upsert(comment("shared", "a.rs", 1, "2026-07-03T10:00:00Z")).unwrap();
    ours.upsert(comment("only-ours", "a.rs", 2, "2026-07-03T10:00:00Z")).unwrap();

    // Their replica saw our first write (clock 1), then edited: seq 2.
    let mut theirs = doc();
    theirs.upsert(comment("shared", "a.rs", 1, "2026-07-03T10:00:00Z")).unwrap();
    let mut newer = comment("shared", "a.rs", 1, "2026-07-03T12:00:00Z");
    newer.text = "their newer edit".into();
    theirs.upsert(newer).unwrap();
    theirs.upsert(comment("only-theirs", "a.rs", 3, "2026-07-03T10:00:00Z")).unwrap();

    ours.merge(&theirs);
    assert_eq!(ours.comments.len(), 3);
    let shared = ours.comments.iter().find(|c| c.id == "shared").unwrap();
    assert_eq!(shared.text, "their newer edit");
    assert_eq!(ours.clock, 3, "clocks join by max");
  }

  #[test]
  fn merge_causally_older_incoming_loses() {
    let mut ours = doc();
    ours.upsert(comment("shared", "a.rs", 1, "2026-07-03T10:00:00Z")).unwrap();
    let mut mine = comment("shared", "a.rs", 1, "2026-07-03T12:00:00Z");
    mine.text = "my newer edit".into();
    ours.upsert(mine).unwrap(); // seq 2

    let mut theirs = doc();
    theirs.upsert(comment("shared", "a.rs", 1, "2026-07-03T10:00:00Z")).unwrap(); // seq 1

    ours.merge(&theirs);
    assert_eq!(ours.comments[0].text, "my newer edit");
  }

  #[test]
  fn merge_is_commutative_on_concurrent_edits() {
    // Both replicas edit the same comment at the same clock — the actor
    // tiebreak must pick the SAME winner regardless of merge direction
    // (the strict-`>` bug this model replaces kept whichever side was
    // local).
    let mut a = doc();
    a.upsert(actor_comment("c", "aaaa", "edit by aaaa")).unwrap();
    let mut b = doc();
    b.upsert(actor_comment("c", "zzzz", "edit by zzzz")).unwrap();

    let mut ab = a.clone();
    ab.merge(&b);
    let mut ba = b.clone();
    ba.merge(&a);
    assert_eq!(ab, ba, "merge direction cannot change the outcome");
    assert_eq!(ab.comments[0].text, "edit by zzzz", "the higher actor wins the tie deterministically");
  }

  #[test]
  fn concurrent_edit_and_resolve_both_survive() {
    // One comment, replicated. A edits the text; B resolves. The registers
    // are independent, so the merged comment is the edited text, resolved.
    let mut origin = doc();
    origin.upsert(actor_comment("c", "aaaa", "original")).unwrap();

    let mut a = origin.clone();
    let mut edited = a.comments[0].clone();
    edited.text = "polished by A".into();
    edited.version.actor = "aaaa".into();
    a.upsert(edited).unwrap();

    let mut b = origin.clone();
    let mut resolved = b.comments[0].clone();
    resolved.resolved_at = Some("2026-07-03T12:00:00Z".into());
    resolved.version.actor = "bbbb".into();
    b.upsert(resolved).unwrap();

    let mut ab = a.clone();
    ab.merge(&b);
    let mut ba = b.clone();
    ba.merge(&a);
    assert_eq!(ab, ba);
    assert_eq!(ab.comments[0].text, "polished by A", "the edit survived");
    assert_eq!(ab.comments[0].resolved_at.as_deref(), Some("2026-07-03T12:00:00Z"), "the resolve survived");
  }

  #[test]
  fn deletion_does_not_resurrect_through_reimport() {
    // The exported copy still carries the comment; deleting locally and
    // importing that copy again must keep it dead — defect #2.
    let mut d = doc();
    d.upsert(comment("c1", "a.rs", 5, "2026-07-03T10:00:00Z")).unwrap();
    let exported = ReviewDocument::parse(&d.to_json_pretty()).unwrap();
    assert!(d.delete("c1"));
    d.merge(&exported);
    assert!(d.comments.is_empty(), "the tombstone dominates the imported copy");
    // And the deletion travels: merging OUR export into the other replica
    // kills the comment there too.
    let mut other = exported.clone();
    other.merge(&d);
    assert!(other.comments.is_empty(), "the deletion propagated");
  }

  #[test]
  fn concurrent_edit_revives_a_deleted_comment() {
    // A deletes; B (not having seen the delete) edits. Both at clock 2.
    // The design keeps the text: revive. Both directions agree.
    let mut origin = doc();
    origin.upsert(actor_comment("c", "aaaa", "original")).unwrap();

    let mut a = origin.clone();
    assert!(a.delete_by("c", "aaaa"));

    let mut b = origin.clone();
    let mut edited = b.comments[0].clone();
    edited.text = "rescued text".into();
    edited.version.actor = "bbbb".into();
    b.upsert(edited).unwrap();

    let mut ab = a.clone();
    ab.merge(&b);
    let mut ba = b.clone();
    ba.merge(&a);
    assert_eq!(ab, ba);
    // Clock-2 edit vs clock-2 delete: equal seq is provably concurrent (a
    // delete that had seen the edit would have ticked past it), so the text
    // survives — regardless of whose actor id sorts higher.
    assert_eq!(ab.comments.len(), 1, "the concurrent edit revived the comment");
    assert_eq!(ab.comments[0].text, "rescued text");
    // A LATER delete (made having seen the edit) wins over it.
    let mut final_delete = ab.clone();
    assert!(final_delete.delete_by("c", "aaaa"));
    let mut replay = ab.clone();
    replay.merge(&final_delete);
    assert!(replay.comments.is_empty(), "a causally-later delete sticks");
  }

  #[test]
  fn merge_laws_hold_across_exchange_orders() {
    // Three replicas from one origin diverge (edit / resolve+add / delete),
    // then exchange pairwise in several different orders; every exchange
    // path must converge to the same document. This is the convergence
    // property that makes exchange order irrelevant in the field.
    let mut origin = doc();
    origin.upsert(actor_comment("shared", "seed", "seed text")).unwrap();

    let mut r1 = origin.clone();
    let mut edited = r1.comments[0].clone();
    edited.text = "r1 edit".into();
    edited.version.actor = "r1".into();
    r1.upsert(edited).unwrap();

    let mut r2 = origin.clone();
    let mut resolved = r2.comments[0].clone();
    resolved.resolved_at = Some("2026-07-03T12:00:00Z".into());
    resolved.version.actor = "r2".into();
    r2.upsert(resolved).unwrap();
    r2.upsert(actor_comment("r2-new", "r2", "r2 addition")).unwrap();

    let mut r3 = origin.clone();
    assert!(r3.delete_by("shared", "r3"));

    let join = |a: &ReviewDocument, b: &ReviewDocument| {
      let mut m = a.clone();
      m.merge(b);
      m
    };
    // Idempotence.
    assert_eq!(join(&r1, &r1), r1);
    // Commutativity.
    assert_eq!(join(&r1, &r2), join(&r2, &r1));
    assert_eq!(join(&r1, &r3), join(&r3, &r1));
    assert_eq!(join(&r2, &r3), join(&r3, &r2));
    // Associativity / convergence: all pairwise exchange orders agree.
    let paths =
      [join(&join(&r1, &r2), &r3), join(&r1, &join(&r2, &r3)), join(&join(&r3, &r1), &r2), join(&join(&r2, &r3), &r1)];
    for p in &paths[1..] {
      assert_eq!(&paths[0], p, "every exchange order converges");
    }
    // And the converged state is sensible: the edit dominates the
    // concurrent delete (revive-keeps-text), the resolve rides along, the
    // addition is present.
    let converged = &paths[0];
    assert_eq!(converged.comments.len(), 2);
    let shared = converged.comments.iter().find(|c| c.id == "shared").unwrap();
    assert_eq!(shared.text, "r1 edit");
    assert!(shared.resolved_at.is_some());
  }

  #[test]
  fn upgraded_v2_documents_merge_as_v2_did() {
    // Two v2 documents (no versions anywhere): newer `updated_at` must
    // still win — the (seq=0, updated_at, actor) key reproduces it.
    let v2 = |updated: &str, text: &str| {
      let mut d = doc();
      d.schema_version = 2;
      let mut c = comment("shared", "a.rs", 1, updated);
      c.text = text.into();
      d.comments.push(c); // bypass upsert: v2 data has no versions
      d
    };
    let older = v2("2026-07-03T10:00:00Z", "older edit");
    let newer = v2("2026-07-03T12:00:00Z", "newer edit");
    let mut ab = older.clone();
    ab.merge(&newer);
    let mut ba = newer.clone();
    ba.merge(&older);
    assert_eq!(ab, ba, "the v2 tie bug does not survive the upgrade");
    assert_eq!(ab.comments[0].text, "newer edit");
    // A post-upgrade write dominates any upgraded write, wall clock aside.
    let mut upgraded = older.clone();
    let mut edit = upgraded.comments[0].clone();
    edit.text = "post-upgrade edit".into();
    edit.updated_at = "2026-07-01T00:00:00Z".into(); // wall-clock OLDER, causally newer
    upgraded.upsert(edit).unwrap();
    let mut m = newer.clone();
    m.merge(&upgraded);
    assert_eq!(m.comments[0].text, "post-upgrade edit");
    assert_eq!(m.schema_version, SCHEMA_VERSION, "merging v3 data modernizes the claim");
  }

  #[test]
  fn parse_rejects_newer_schema() {
    let mut d = doc();
    d.schema_version = SCHEMA_VERSION + 1;
    let json = serde_json::to_string(&d).unwrap();
    assert_eq!(
      ReviewDocument::parse(&json),
      Err(ModelError::UnsupportedSchema { found: SCHEMA_VERSION + 1, supported: SCHEMA_VERSION })
    );
  }

  #[test]
  fn parse_rejects_garbage_and_invalid_comments() {
    assert!(matches!(ReviewDocument::parse("not json"), Err(ModelError::Json(_))));
    let mut d = doc();
    d.comments.push(Comment {
      id: "".into(),
      file: "a.rs".into(),
      side: Side::New,
      line: 1,
      text: "x".into(),
      created_at: "t".into(),
      updated_at: "t".into(),
      resolved_at: None,
      version: Version::default(),
      resolution_version: Version::default(),
    });
    let json = serde_json::to_string(&d).unwrap();
    assert!(matches!(ReviewDocument::parse(&json), Err(ModelError::InvalidComment(_))));
  }

  #[test]
  fn parse_normalizes_order_and_absorbs_runaway_clocks() {
    let mut d = doc();
    d.comments.push(comment("b", "z.rs", 9, "2026-07-03T10:00:00Z"));
    d.comments.push(comment("a", "a.rs", 1, "2026-07-03T10:00:00Z"));
    d.comments[0].version = Version { seq: 41, actor: "x".into() };
    let parsed = ReviewDocument::parse(&serde_json::to_string(&d).unwrap()).unwrap();
    assert_eq!(parsed.comments[0].id, "a");
    assert_eq!(parsed.clock, 41, "a hand-raised version cannot outrun the clock");
  }

  #[test]
  fn verdict_serializes_as_single_key_union_with_timestamp() {
    assert_eq!(
      serde_json::to_value(verdict_approved("2026-07-13T10:00:00Z")).unwrap(),
      serde_json::json!({ "Approved": { "at": "2026-07-13T10:00:00Z" } })
    );
    assert_eq!(
      serde_json::to_value(Verdict::ChangesRequired { at: "t".into() }).unwrap(),
      serde_json::json!({ "ChangesRequired": { "at": "t" } })
    );
  }

  #[test]
  fn set_verdict_validates_and_clears() {
    let mut d = doc();
    assert!(matches!(d.set_verdict(Some(verdict_approved("  "))), Err(ModelError::InvalidVerdict(_))));
    assert_eq!(d.verdict, None, "a rejected verdict leaves the document untouched");
    assert_eq!(d.clock, 0, "and does not advance the clock");
    d.set_verdict(Some(verdict_approved("2026-07-13T10:00:00Z"))).unwrap();
    assert_eq!(d.verdict.as_ref().unwrap().label(), "approved");
    d.set_verdict(None).unwrap();
    assert_eq!(d.verdict, None, "clearing returns the review to in-progress");
    assert_eq!(d.verdict_version.seq, 2, "clearing is a stamped write — it merges");
  }

  #[test]
  fn merge_verdict_causally_later_decision_wins_and_clears_propagate() {
    // A decision beats no decision.
    let mut ours = doc();
    let mut theirs = doc();
    theirs.set_verdict(Some(verdict_approved("2026-07-13T10:00:00Z"))).unwrap();
    ours.merge(&theirs);
    assert_eq!(ours.verdict, theirs.verdict);
    // A causally-later decision beats an earlier one.
    let mut newer = ours.clone();
    newer.set_verdict(Some(Verdict::ChangesRequired { at: "2026-07-13T12:00:00Z".into() })).unwrap();
    ours.merge(&newer);
    assert_eq!(ours.verdict.as_ref().unwrap().label(), "changes required");
    // An earlier decision loses.
    ours.merge(&theirs);
    assert_eq!(ours.verdict, newer.verdict);
    // A causally-later CLEAR also wins — the v2 model could not express
    // this; imports silently un-cleared.
    let mut cleared = ours.clone();
    cleared.set_verdict(None).unwrap();
    ours.merge(&cleared);
    assert_eq!(ours.verdict, None, "the clear propagated through merge");
    // But merging a stale decision back does not resurrect it.
    ours.merge(&newer);
    assert_eq!(ours.verdict, None);
  }

  #[test]
  fn upgraded_v2_verdicts_merge_as_v2_did() {
    // Both sides zero-versioned: later `at` wins; no decision never beats
    // a decision — exactly the v2 rules, minus the tie bug.
    let mut with = doc();
    with.verdict = Some(verdict_approved("2026-07-13T10:00:00Z")); // bypass set_verdict: v2 data
    let without = doc();
    let mut m = without.clone();
    m.merge(&with);
    assert_eq!(m.verdict, with.verdict, "a decision beats no decision");
    let mut m2 = with.clone();
    m2.merge(&without);
    assert_eq!(m2.verdict, with.verdict, "and no decision never clears one, for v2 data");
  }

  #[test]
  fn resolved_comments_validate_and_roundtrip() {
    let mut d = doc();
    let mut resolved = comment("c1", "a.rs", 5, "2026-07-13T11:00:00Z");
    resolved.resolved_at = Some("2026-07-13T11:00:00Z".into());
    d.upsert(resolved).unwrap();
    let back = ReviewDocument::parse(&d.to_json_pretty()).unwrap();
    assert_eq!(back.comments[0].resolved_at.as_deref(), Some("2026-07-13T11:00:00Z"));
    // Present-but-blank is rejected; absent stays absent in the JSON.
    let mut blank = comment("c2", "a.rs", 6, "t");
    blank.resolved_at = Some("  ".into());
    assert!(matches!(d.upsert(blank), Err(ModelError::InvalidComment(_))));
    let open = comment("c3", "a.rs", 7, "t");
    d.upsert(open).unwrap();
    assert!(!serde_json::to_string(&d.comments[1]).unwrap().contains("resolved_at"));
  }

  #[test]
  fn v1_documents_parse_with_the_new_fields_defaulted() {
    // A document exactly as a v1 page wrote it: no verdict, no resolved_at,
    // and (like v2) no versions, no clock, no tombstones.
    let v1 = r#"{
      "schema_version": 1, "tool": "packdiff", "repo": "repo",
      "base": { "name": "main", "sha": "a" }, "head": { "name": "feat", "sha": "b" },
      "comments": [ { "id": "c1", "file": "a.rs", "side": "New", "line": 5,
        "text": "note", "created_at": "t", "updated_at": "t" } ]
    }"#;
    let doc = ReviewDocument::parse(v1).unwrap();
    assert_eq!(doc.verdict, None);
    assert_eq!(doc.comments[0].resolved_at, None);
    assert!(doc.comments[0].version.is_zero());
    assert_eq!(doc.clock, 0);
    assert!(doc.tombstones.is_empty());
    assert_eq!(doc.schema_version, 1, "parsing alone never modernizes the claim");
  }

  #[test]
  fn canonical_json_roundtrips_and_zero_versions_stay_hidden() {
    let mut d = doc();
    d.upsert(comment("c1", "a.rs", 5, "2026-07-03T10:00:00Z")).unwrap();
    let json = d.to_json_pretty();
    assert!(json.ends_with('\n'));
    assert!(json.contains("\"version\""), "a real write serializes its version");
    assert!(json.contains("\"clock\""), "and the clock");
    let back = ReviewDocument::parse(&json).unwrap();
    assert_eq!(back, d);
    // An untouched document serializes without any v3 field — its bytes
    // stay readable by (and absorbable from) a v2 page.
    let untouched = doc();
    let json = untouched.to_json_pretty();
    for absent in ["clock", "tombstones", "resolution_version", "verdict_version"] {
      assert!(!json.contains(absent), "an untouched document must not serialize `{absent}`");
    }
    assert_eq!(json.matches("version").count(), 1, "only `schema_version` mentions versions");
  }
}
