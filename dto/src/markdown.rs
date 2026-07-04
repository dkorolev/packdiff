//! Minimal, safety-first Markdown → HTML: used for comment bodies and for
//! the rendered view of markdown files on the generated page.
//!
//! Safety by construction: every input character is HTML-escaped first; the
//! only tags in the output are the ones this module emits, and link targets
//! are restricted to `http://` / `https://` / `mailto:` — hostile input
//! cannot smuggle markup or `javascript:` URLs.
//!
//! Deliberately a SUBSET (documented in docs/page.md): ATX headings, fenced
//! code blocks, flat (non-nested) lists, blockquotes, thematic breaks,
//! paragraphs; inline `` `code` ``, `**bold**`, `*italic*`, and
//! `[links](https://…)`. Underscores are NOT emphasis, so `snake_case`
//! identifiers survive verbatim. A single newline inside a paragraph is a
//! hard break, matching how people write review comments.

/// Render markdown `text` to HTML. Pure and deterministic; never fails —
/// anything unrecognized falls back to escaped literal text.
pub fn to_html(text: &str) -> String {
  let lines: Vec<&str> = text.lines().collect();
  blocks_to_html(&lines, 0)
}

/// Recursion cap for nested blockquotes and nested inline emphasis; markdown
/// past this depth renders as escaped literal text instead of overflowing.
const MAX_DEPTH: u32 = 8;

fn esc(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  for ch in s.chars() {
    esc_char(ch, &mut out);
  }
  out
}

fn esc_char(ch: char, out: &mut String) {
  match ch {
    '&' => out.push_str("&amp;"),
    '<' => out.push_str("&lt;"),
    '>' => out.push_str("&gt;"),
    '"' => out.push_str("&quot;"),
    '\'' => out.push_str("&#x27;"),
    _ => out.push(ch),
  }
}

fn blocks_to_html(lines: &[&str], depth: u32) -> String {
  let mut out = String::new();
  let mut i = 0;
  while i < lines.len() {
    let trimmed = lines[i].trim_start();
    if trimmed.is_empty() {
      i += 1;
      continue;
    }
    if trimmed.starts_with("```") {
      // Fenced code block; an unclosed fence runs to the end of the input.
      let mut j = i + 1;
      let mut code = String::new();
      while j < lines.len() && !lines[j].trim_start().starts_with("```") {
        code.push_str(lines[j]);
        code.push('\n');
        j += 1;
      }
      out.push_str(&format!("<pre><code>{}</code></pre>", esc(&code)));
      i = j + 1;
      continue;
    }
    if let Some((level, content)) = heading(trimmed) {
      out.push_str(&format!("<h{level}>{}</h{level}>", inline(content, depth)));
      i += 1;
      continue;
    }
    if is_rule(trimmed) {
      out.push_str("<hr>");
      i += 1;
      continue;
    }
    if trimmed.starts_with('>') && depth < MAX_DEPTH {
      let mut inner: Vec<&str> = Vec::new();
      while i < lines.len() {
        let t = lines[i].trim_start();
        let Some(stripped) = t.strip_prefix('>') else { break };
        inner.push(stripped.strip_prefix(' ').unwrap_or(stripped));
        i += 1;
      }
      out.push_str(&format!("<blockquote>{}</blockquote>", blocks_to_html(&inner, depth + 1)));
      continue;
    }
    if unordered_item(trimmed).is_some() {
      out.push_str("<ul>");
      while i < lines.len() {
        let Some(item) = unordered_item(lines[i].trim_start()) else { break };
        out.push_str(&format!("<li>{}</li>", inline(item, depth)));
        i += 1;
      }
      out.push_str("</ul>");
      continue;
    }
    if ordered_item(trimmed).is_some() {
      out.push_str("<ol>");
      while i < lines.len() {
        let Some(item) = ordered_item(lines[i].trim_start()) else { break };
        out.push_str(&format!("<li>{}</li>", inline(item, depth)));
        i += 1;
      }
      out.push_str("</ol>");
      continue;
    }
    // Paragraph: consecutive plain lines; each single newline is a hard break.
    let mut parts: Vec<String> = Vec::new();
    while i < lines.len() {
      let t = lines[i].trim_start();
      if t.is_empty() || starts_block(t) {
        break;
      }
      parts.push(inline(lines[i].trim_end(), depth));
      i += 1;
    }
    out.push_str(&format!("<p>{}</p>", parts.join("<br>")));
  }
  out
}

/// True when the line begins some non-paragraph block, ending a paragraph.
fn starts_block(trimmed: &str) -> bool {
  trimmed.starts_with("```")
    || trimmed.starts_with('>')
    || heading(trimmed).is_some()
    || is_rule(trimmed)
    || unordered_item(trimmed).is_some()
    || ordered_item(trimmed).is_some()
}

/// `#{1,6} ` → `(level, content)`.
fn heading(trimmed: &str) -> Option<(usize, &str)> {
  let level = trimmed.bytes().take_while(|&b| b == b'#').count();
  if (1..=6).contains(&level) {
    if let Some(content) = trimmed[level..].strip_prefix(' ') {
      return Some((level, content.trim()));
    }
  }
  None
}

/// Three or more of the same `-` / `*` / `_` and nothing else.
fn is_rule(trimmed: &str) -> bool {
  let t: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
  t.len() >= 3 && (t.chars().all(|c| c == '-') || t.chars().all(|c| c == '*') || t.chars().all(|c| c == '_'))
}

/// `- item` / `* item` / `+ item` → the item text.
fn unordered_item(trimmed: &str) -> Option<&str> {
  for marker in ["- ", "* ", "+ "] {
    if let Some(rest) = trimmed.strip_prefix(marker) {
      return Some(rest);
    }
  }
  None
}

/// `12. item` → the item text.
fn ordered_item(trimmed: &str) -> Option<&str> {
  let digits = trimmed.bytes().take_while(|b| b.is_ascii_digit()).count();
  if digits == 0 {
    return None;
  }
  trimmed[digits..].strip_prefix(". ")
}

/// Emphasis content must be non-empty and not whitespace-flanked, so a bare
/// asterisk in prose (`a * b`) stays literal.
fn emphasizable(inner: &str) -> bool {
  !inner.is_empty() && inner.trim() == inner
}

/// True for link targets this module will emit as `href`.
fn safe_url(url: &str) -> bool {
  url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:")
}

/// Inline spans: `` `code` `` (wins over everything inside it), `**bold**`,
/// `*italic*`, `[label](url)`. Anything unmatched stays escaped literal text.
fn inline(text: &str, depth: u32) -> String {
  if depth > MAX_DEPTH {
    return esc(text);
  }
  let mut out = String::new();
  let mut i = 0;
  while i < text.len() {
    let rest = &text[i..];
    if let Some(after) = rest.strip_prefix('`') {
      if let Some(n) = after.find('`') {
        out.push_str(&format!("<code>{}</code>", esc(&after[..n])));
        i += n + 2;
        continue;
      }
    }
    if let Some(after) = rest.strip_prefix("**") {
      if let Some(mut n) = after.find("**") {
        // `**outer *inner***`: the first `**` found sits inside the trailing
        // `***`; shift by one so the inner `*…*` pair stays intact.
        if after[n..].starts_with("***") {
          n += 1;
        }
        if emphasizable(&after[..n]) {
          out.push_str(&format!("<strong>{}</strong>", inline(&after[..n], depth + 1)));
          i += n + 4;
          continue;
        }
      }
    }
    if let Some(after) = rest.strip_prefix('*') {
      if let Some(n) = after.find('*') {
        if emphasizable(&after[..n]) {
          out.push_str(&format!("<em>{}</em>", inline(&after[..n], depth + 1)));
          i += n + 2;
          continue;
        }
      }
    }
    if rest.starts_with('[') {
      if let Some(close) = rest.find("](") {
        if let Some(end) = rest[close + 2..].find(')') {
          let label = &rest[1..close];
          let url = &rest[close + 2..close + 2 + end];
          if safe_url(url) {
            out.push_str(&format!(r#"<a href="{}">{}</a>"#, esc(url), inline(label, depth + 1)));
            i += close + 2 + end + 1;
            continue;
          }
        }
      }
    }
    // `text[i..]` is always on a char boundary: every arm advances by whole
    // characters (ASCII markers or a full `len_utf8`).
    let ch = rest.chars().next().expect("rest is non-empty inside the loop");
    esc_char(ch, &mut out);
    i += ch.len_utf8();
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn hostile_html_is_escaped_everywhere() {
    assert_eq!(to_html("<script>alert(1)</script>"), "<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>");
    assert_eq!(to_html("# <b>hi</b>"), "<h1>&lt;b&gt;hi&lt;/b&gt;</h1>");
    assert_eq!(to_html("```\n<script>x</script>\n```"), "<pre><code>&lt;script&gt;x&lt;/script&gt;\n</code></pre>");
    assert_eq!(to_html("`<i>`"), "<p><code>&lt;i&gt;</code></p>");
  }

  #[test]
  fn unsafe_link_schemes_stay_literal_text() {
    let js = to_html("[x](javascript:alert(1))");
    assert!(!js.contains("<a "), "{js}");
    assert!(js.contains("javascript:alert(1)"));
    let ok = to_html("[docs](https://example.com/a?b=1)");
    assert_eq!(ok, r#"<p><a href="https://example.com/a?b=1">docs</a></p>"#);
  }

  #[test]
  fn headings_paragraphs_and_hard_breaks() {
    assert_eq!(to_html("## Title"), "<h2>Title</h2>");
    assert_eq!(to_html("####### seven"), "<p>####### seven</p>");
    assert_eq!(to_html("line one\nline two"), "<p>line one<br>line two</p>");
    assert_eq!(to_html("para one\n\npara two"), "<p>para one</p><p>para two</p>");
  }

  #[test]
  fn emphasis_code_and_snake_case_survival() {
    assert_eq!(
      to_html("**bold** and *em* and `code`"),
      "<p><strong>bold</strong> and <em>em</em> and <code>code</code></p>"
    );
    assert_eq!(to_html("**outer *inner***"), "<p><strong>outer <em>inner</em></strong></p>");
    assert_eq!(to_html("keep snake_case and __this__ literal"), "<p>keep snake_case and __this__ literal</p>");
    assert_eq!(to_html("a * b stays literal"), "<p>a * b stays literal</p>");
  }

  #[test]
  fn lists_blockquotes_and_rules() {
    assert_eq!(to_html("- a\n- b"), "<ul><li>a</li><li>b</li></ul>");
    assert_eq!(to_html("1. a\n2. b"), "<ol><li>a</li><li>b</li></ol>");
    assert_eq!(to_html("> quoted\n> more"), "<blockquote><p>quoted<br>more</p></blockquote>");
    assert_eq!(to_html("---"), "<hr>");
  }

  #[test]
  fn unclosed_fence_runs_to_the_end() {
    assert_eq!(to_html("```\ncode"), "<pre><code>code\n</code></pre>");
  }

  #[test]
  fn multibyte_text_is_preserved() {
    assert_eq!(to_html("héllo — **wörld** 🚀"), "<p>héllo — <strong>wörld</strong> 🚀</p>");
  }
}
