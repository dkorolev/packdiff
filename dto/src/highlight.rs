//! Safe, dependency-free lexical syntax highlighting.
//!
//! One generic scanner is parameterized by static language profiles. The
//! output is safe HTML: source text is escaped here, and the only markup this
//! module emits is its own fixed `tok-*` spans.

use crate::diff::Line;

#[derive(Clone, Copy)]
struct StringKind {
  delim: &'static str,
  escapes: bool,
  multiline: bool,
}

struct Profile {
  line_comments: &'static [&'static str],
  block_comment: Option<(&'static str, &'static str)>,
  strings: &'static [StringKind],
  keywords: &'static [&'static str],
  literals: &'static [&'static str],
  fn_call: bool,
}

#[derive(Clone, Copy)]
enum State {
  Neutral,
  BlockComment(&'static str),
  String(StringKind),
}

struct Scanner {
  profile: &'static Profile,
  state: State,
}

impl Scanner {
  fn new(profile: &'static Profile) -> Self {
    Self { profile, state: State::Neutral }
  }

  fn line(&mut self, input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut pos = 0;
    while pos < input.len() {
      match self.state {
        State::BlockComment(end) => {
          let close = input[pos..].find(end).map(|i| pos + i + end.len());
          let stop = close.unwrap_or(input.len());
          token(&mut out, "tok-com", &input[pos..stop]);
          pos = stop;
          if close.is_some() {
            self.state = State::Neutral;
          }
        }
        State::String(kind) => {
          let (stop, closed) = string_end(input, pos, kind, false);
          token(&mut out, "tok-str", &input[pos..stop]);
          pos = stop;
          if closed {
            self.state = State::Neutral;
          }
        }
        State::Neutral => {
          if self.profile.line_comments.iter().any(|marker| input[pos..].starts_with(*marker)) {
            token(&mut out, "tok-com", &input[pos..]);
            break;
          }
          if let Some((start, end)) = self.profile.block_comment.filter(|(start, _)| input[pos..].starts_with(start)) {
            let close = input[pos + start.len()..].find(end).map(|i| pos + start.len() + i + end.len());
            let stop = close.unwrap_or(input.len());
            token(&mut out, "tok-com", &input[pos..stop]);
            pos = stop;
            if close.is_none() {
              self.state = State::BlockComment(end);
            }
            continue;
          }
          if let Some(kind) = self.profile.strings.iter().find(|kind| input[pos..].starts_with(kind.delim)).copied() {
            let (stop, closed) = string_end(input, pos, kind, true);
            token(&mut out, "tok-str", &input[pos..stop]);
            pos = stop;
            if !closed && kind.multiline {
              self.state = State::String(kind);
            }
            continue;
          }
          let ch = input[pos..].chars().next().expect("pos is within the input");
          if ch.is_ascii_digit() || (ch == '.' && input[pos + 1..].starts_with(|c: char| c.is_ascii_digit())) {
            let stop = number_end(input, pos);
            token(&mut out, "tok-num", &input[pos..stop]);
            pos = stop;
            continue;
          }
          if ident_start(ch) {
            let stop = ident_end(input, pos);
            let word = &input[pos..stop];
            let class = if self.profile.keywords.binary_search(&word).is_ok() {
              Some("tok-kw")
            } else if self.profile.literals.binary_search(&word).is_ok() {
              Some("tok-lit")
            } else if self.profile.fn_call && input[stop..].trim_start().starts_with('(') {
              Some("tok-fn")
            } else {
              None
            };
            if let Some(class) = class {
              token(&mut out, class, word);
            } else {
              escape_into(&mut out, word);
            }
            pos = stop;
            continue;
          }
          escape_char(&mut out, ch);
          pos += ch.len_utf8();
        }
      }
    }
    out
  }
}

fn string_end(input: &str, start: usize, kind: StringKind, opening: bool) -> (usize, bool) {
  let mut pos = start + if opening { kind.delim.len() } else { 0 };
  let mut escaped = false;
  while pos < input.len() {
    if !escaped && input[pos..].starts_with(kind.delim) {
      return (pos + kind.delim.len(), true);
    }
    let ch = input[pos..].chars().next().expect("pos is within the input");
    escaped = kind.escapes && ch == '\\' && !escaped;
    pos += ch.len_utf8();
  }
  (input.len(), false)
}

fn ident_start(ch: char) -> bool {
  ch == '_' || ch.is_alphabetic()
}

fn ident_end(input: &str, start: usize) -> usize {
  input[start..]
    .char_indices()
    .find(|(i, ch)| *i > 0 && !(*ch == '_' || ch.is_alphanumeric()))
    .map_or(input.len(), |(i, _)| start + i)
}

fn number_end(input: &str, start: usize) -> usize {
  let mut end = start;
  let mut previous = '\0';
  for (i, ch) in input[start..].char_indices() {
    let allowed = ch.is_ascii_alphanumeric()
      || matches!(ch, '_' | '.')
      || (matches!(ch, '+' | '-') && matches!(previous, 'e' | 'E' | 'p' | 'P'));
    if !allowed {
      break;
    }
    end = start + i + ch.len_utf8();
    previous = ch;
  }
  end
}

fn token(out: &mut String, class: &str, text: &str) {
  out.push_str("<span class=\"");
  out.push_str(class);
  out.push_str("\">");
  escape_into(out, text);
  out.push_str("</span>");
}

fn escape_into(out: &mut String, text: &str) {
  for ch in text.chars() {
    escape_char(out, ch);
  }
}

fn escape_char(out: &mut String, ch: char) {
  match ch {
    '&' => out.push_str("&amp;"),
    '<' => out.push_str("&lt;"),
    '>' => out.push_str("&gt;"),
    '"' => out.push_str("&quot;"),
    '\'' => out.push_str("&#39;"),
    _ => out.push(ch),
  }
}

/// Highlight a contiguous run of source lines. Unknown paths return `None`,
/// allowing callers to preserve their plain escaped rendering.
pub fn highlight_lines(path: &str, lines: &[&str]) -> Option<Vec<String>> {
  let mut scanner = Scanner::new(profile_for(path)?);
  Some(lines.iter().map(|line| scanner.line(line)).collect())
}

/// Highlight a diff hunk with independent lexer state for its old and new
/// sides. Context advances both streams and renders from the new side.
pub fn highlight_hunk(path: &str, lines: &[Line]) -> Option<Vec<String>> {
  let profile = profile_for(path)?;
  let mut old = Scanner::new(profile);
  let mut new = Scanner::new(profile);
  Some(
    lines
      .iter()
      .map(|line| match line {
        Line::Add { text, .. } => new.line(text),
        Line::Del { text, .. } => old.line(text),
        Line::Ctx { text, .. } => {
          old.line(text);
          new.line(text)
        }
        Line::Meta { text } => {
          let mut html = String::new();
          escape_into(&mut html, text);
          html
        }
      })
      .collect(),
  )
}

const DQ: StringKind = StringKind { delim: "\"", escapes: true, multiline: false };
const SQ: StringKind = StringKind { delim: "'", escapes: true, multiline: false };
const RAW: StringKind = StringKind { delim: "`", escapes: false, multiline: true };
const TRIPLE_DQ: StringKind = StringKind { delim: "\"\"\"", escapes: true, multiline: true };
const TRIPLE_SQ: StringKind = StringKind { delim: "'''", escapes: true, multiline: true };

const C_LIKE: Profile = Profile {
  line_comments: &["//"],
  block_comment: Some(("/*", "*/")),
  strings: &[DQ, SQ],
  keywords: &[
    "alignas",
    "alignof",
    "asm",
    "auto",
    "bool",
    "break",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "constexpr",
    "continue",
    "default",
    "delete",
    "do",
    "double",
    "else",
    "enum",
    "explicit",
    "export",
    "extern",
    "float",
    "for",
    "friend",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "namespace",
    "new",
    "operator",
    "private",
    "protected",
    "public",
    "register",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "struct",
    "switch",
    "template",
    "this",
    "throw",
    "try",
    "typedef",
    "typename",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "volatile",
    "while",
  ],
  literals: &["NULL", "false", "nullptr", "true"],
  fn_call: true,
};
const RUST: Profile = Profile {
  line_comments: &["//"],
  block_comment: Some(("/*", "*/")),
  strings: &[DQ, SQ],
  keywords: &[
    "Self", "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "fn",
    "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "static",
    "struct", "super", "trait", "type", "unsafe", "use", "where", "while",
  ],
  literals: &["None", "false", "true"],
  fn_call: true,
};
const JAVA: Profile = Profile {
  line_comments: &["//"],
  block_comment: Some(("/*", "*/")),
  strings: &[DQ, SQ],
  keywords: &[
    "abstract",
    "assert",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "final",
    "finally",
    "float",
    "for",
    "goto",
    "if",
    "implements",
    "import",
    "instanceof",
    "int",
    "interface",
    "long",
    "native",
    "new",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "static",
    "strictfp",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "transient",
    "try",
    "void",
    "volatile",
    "while",
  ],
  literals: &["false", "null", "true"],
  fn_call: true,
};
const KOTLIN: Profile = Profile {
  line_comments: &["//"],
  block_comment: Some(("/*", "*/")),
  strings: &[TRIPLE_DQ, DQ, SQ],
  keywords: &[
    "as",
    "break",
    "by",
    "catch",
    "class",
    "constructor",
    "continue",
    "data",
    "do",
    "else",
    "enum",
    "finally",
    "for",
    "fun",
    "if",
    "import",
    "in",
    "interface",
    "is",
    "object",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "sealed",
    "super",
    "this",
    "throw",
    "try",
    "typealias",
    "val",
    "var",
    "when",
    "while",
  ],
  literals: &["false", "null", "true"],
  fn_call: true,
};
const GO: Profile = Profile {
  line_comments: &["//"],
  block_comment: Some(("/*", "*/")),
  strings: &[RAW, DQ, SQ],
  keywords: &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "fallthrough",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "type",
    "var",
  ],
  literals: &["false", "nil", "true"],
  fn_call: true,
};
const PYTHON: Profile = Profile {
  line_comments: &["#"],
  block_comment: None,
  strings: &[TRIPLE_DQ, TRIPLE_SQ, DQ, SQ],
  keywords: &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except",
    "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise",
    "return", "try", "while", "with", "yield",
  ],
  literals: &["False", "None", "True"],
  fn_call: true,
};
const JS: Profile = Profile {
  line_comments: &["//"],
  block_comment: Some(("/*", "*/")),
  strings: &[RAW, DQ, SQ],
  keywords: &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "finally",
    "for",
    "from",
    "function",
    "get",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "of",
    "return",
    "set",
    "static",
    "super",
    "switch",
    "throw",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
  ],
  literals: &["false", "null", "true", "undefined"],
  fn_call: true,
};
const RUBY: Profile = Profile {
  line_comments: &["#"],
  block_comment: None,
  strings: &[DQ, SQ],
  keywords: &[
    "BEGIN", "END", "alias", "begin", "break", "case", "class", "def", "defined", "do", "else", "elsif", "end",
    "ensure", "for", "if", "in", "module", "next", "redo", "rescue", "retry", "return", "self", "super", "then",
    "undef", "unless", "until", "when", "while", "yield",
  ],
  literals: &["false", "nil", "true"],
  fn_call: true,
};
const SHELL: Profile = Profile {
  line_comments: &["#"],
  block_comment: None,
  strings: &[DQ, SQ],
  keywords: &[
    "case", "do", "done", "elif", "else", "esac", "fi", "for", "function", "if", "in", "select", "then", "time",
    "until", "while",
  ],
  literals: &["false", "true"],
  fn_call: true,
};
const SQL: Profile = Profile {
  line_comments: &["--"],
  block_comment: Some(("/*", "*/")),
  strings: &[SQ, DQ],
  keywords: &[
    "ADD",
    "ALTER",
    "AND",
    "AS",
    "ASC",
    "BEGIN",
    "BETWEEN",
    "BY",
    "CASE",
    "CREATE",
    "DELETE",
    "DESC",
    "DISTINCT",
    "DROP",
    "ELSE",
    "END",
    "EXISTS",
    "FROM",
    "FULL",
    "GROUP",
    "HAVING",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTO",
    "IS",
    "JOIN",
    "LEFT",
    "LIKE",
    "LIMIT",
    "NOT",
    "ON",
    "OR",
    "ORDER",
    "OUTER",
    "PRIMARY",
    "REFERENCES",
    "RIGHT",
    "SELECT",
    "SET",
    "TABLE",
    "THEN",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "VALUES",
    "WHEN",
    "WHERE",
  ],
  literals: &["FALSE", "NULL", "TRUE"],
  fn_call: true,
};
const CSS: Profile = Profile {
  line_comments: &[],
  block_comment: Some(("/*", "*/")),
  strings: &[DQ, SQ],
  keywords: &[],
  literals: &[],
  fn_call: true,
};
const TOML: Profile = Profile {
  line_comments: &["#"],
  block_comment: None,
  strings: &[TRIPLE_DQ, TRIPLE_SQ, DQ, SQ],
  keywords: &[],
  literals: &["false", "true"],
  fn_call: false,
};
const YAML: Profile = Profile {
  line_comments: &["#"],
  block_comment: None,
  strings: &[DQ, SQ],
  keywords: &[],
  literals: &["false", "null", "true", "~"],
  fn_call: false,
};
const JSON: Profile = Profile {
  line_comments: &[],
  block_comment: None,
  strings: &[DQ],
  keywords: &[],
  literals: &["false", "null", "true"],
  fn_call: false,
};

fn profile_for(path: &str) -> Option<&'static Profile> {
  let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
  match ext.as_str() {
    "rs" => Some(&RUST),
    "c" | "cc" | "cpp" | "h" | "hpp" => Some(&C_LIKE),
    "java" => Some(&JAVA),
    "kt" | "kts" => Some(&KOTLIN),
    "go" => Some(&GO),
    "py" => Some(&PYTHON),
    "js" | "jsx" | "mjs" | "ts" | "tsx" => Some(&JS),
    "rb" => Some(&RUBY),
    "bash" | "sh" | "zsh" => Some(&SHELL),
    "sql" => Some(&SQL),
    "css" => Some(&CSS),
    "toml" => Some(&TOML),
    "yaml" | "yml" => Some(&YAML),
    "json" => Some(&JSON),
    _ => None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn one(path: &str, line: &str) -> String {
    highlight_lines(path, &[line]).expect("test path has a profile").remove(0)
  }

  #[test]
  fn profile_smoke_tests_cover_every_language_family() {
    let cases: [(&str, &str, &[&str]); 14] = [
      (
        "x.rs",
        "fn main() { call(1, \"x\"); true } // hi",
        &["tok-kw", "tok-fn", "tok-num", "tok-str", "tok-lit", "tok-com"],
      ),
      (
        "x.cpp",
        "return call(0xff, \"x\", true); // hi",
        &["tok-kw", "tok-fn", "tok-num", "tok-str", "tok-lit", "tok-com"],
      ),
      (
        "x.java",
        "class X { call(1, \"x\", true); } // hi",
        &["tok-kw", "tok-fn", "tok-num", "tok-str", "tok-lit", "tok-com"],
      ),
      (
        "x.kt",
        "fun main() = call(1, \"x\", true) // hi",
        &["tok-kw", "tok-fn", "tok-num", "tok-str", "tok-lit", "tok-com"],
      ),
      (
        "x.go",
        "func main() { call(1, \"x\", nil) } // hi",
        &["tok-kw", "tok-fn", "tok-num", "tok-str", "tok-lit", "tok-com"],
      ),
      (
        "x.py",
        "def f(): return call(1, \"x\", None) # hi",
        &["tok-kw", "tok-fn", "tok-num", "tok-str", "tok-lit", "tok-com"],
      ),
      (
        "x.tsx",
        "const x = call(1, \"x\", undefined); // hi",
        &["tok-kw", "tok-fn", "tok-num", "tok-str", "tok-lit", "tok-com"],
      ),
      (
        "x.rb",
        "def f; call(1, \"x\", nil); end # hi",
        &["tok-kw", "tok-fn", "tok-num", "tok-str", "tok-lit", "tok-com"],
      ),
      (
        "x.sh",
        "if call(1, \"x\", true); then echo ok; fi # hi",
        &["tok-kw", "tok-fn", "tok-num", "tok-str", "tok-lit", "tok-com"],
      ),
      (
        "x.sql",
        "SELECT call(1, 'x', NULL) FROM t -- hi",
        &["tok-kw", "tok-fn", "tok-num", "tok-str", "tok-lit", "tok-com"],
      ),
      ("x.css", "a { width: calc(12px); content: \"x\"; } /* hi */", &["tok-fn", "tok-num", "tok-str", "tok-com"]),
      ("x.toml", "value = \"x\"; count = 1; enabled = true # hi", &["tok-str", "tok-num", "tok-lit", "tok-com"]),
      ("x.yaml", "value: \"x\", count: 1, enabled: true # hi", &["tok-str", "tok-num", "tok-lit", "tok-com"]),
      ("x.json", "{\"value\": 1, \"ok\": true}", &["tok-str", "tok-num", "tok-lit"]),
    ];
    for (path, source, expected) in cases {
      let html = one(path, source);
      for class in expected {
        assert!(html.contains(class), "{path} should contain {class}: {html}");
      }
    }
  }

  #[test]
  fn profile_word_lists_stay_sorted_for_binary_search() {
    for profile in [&RUST, &C_LIKE, &JAVA, &KOTLIN, &GO, &PYTHON, &JS, &RUBY, &SHELL, &SQL, &CSS, &TOML, &YAML, &JSON] {
      assert!(profile.keywords.windows(2).all(|pair| pair[0] <= pair[1]));
      assert!(profile.literals.windows(2).all(|pair| pair[0] <= pair[1]));
    }
  }

  #[test]
  fn state_carries_across_multiline_constructs() {
    let rust = highlight_lines("x.rs", &["/* open", "still */ let x = 1;"]).expect("known profile");
    assert!(rust[1].starts_with("<span class=\"tok-com\">still */</span> <span class=\"tok-kw\">let</span>"));
    let python = highlight_lines("x.py", &["\"\"\"open", "still", "\"\"\" + 2"]).expect("known profile");
    assert!(python[1].starts_with("<span class=\"tok-str\">still</span>"));
    assert!(python[2].starts_with("<span class=\"tok-str\">&quot;&quot;&quot;</span>"));
  }

  #[test]
  fn hunk_keeps_old_and_new_state_separate() {
    let lines = vec![
      Line::Del { old: 1, text: "/* old opens".into() },
      Line::Add { new: 1, text: "new is plain */".into() },
      Line::Del { old: 2, text: "old closes */".into() },
      Line::Add { new: 2, text: "let value = true;".into() },
    ];
    let html = highlight_hunk("x.rs", &lines).expect("known profile");
    assert!(html[2].starts_with("<span class=\"tok-com\">"));
    assert!(html[3].starts_with("<span class=\"tok-kw\">let</span>"));
  }

  #[test]
  fn unknown_and_deliberately_plain_paths_fall_back() {
    assert_eq!(highlight_lines("README.md", &["# heading"]), None);
    assert_eq!(highlight_lines("page.html", &["<b>x</b>"]), None);
  }

  #[test]
  fn hostile_input_is_escaped_and_spans_preserve_exact_escaped_text() {
    let source = "const x = \"</span><script>&'\\\"\"; // <evil>";
    let html = one("x.js", source);
    assert!(!html.contains("<script>"));
    let stripped = html
      .replace("<span class=\"tok-com\">", "")
      .replace("<span class=\"tok-str\">", "")
      .replace("<span class=\"tok-num\">", "")
      .replace("<span class=\"tok-kw\">", "")
      .replace("<span class=\"tok-lit\">", "")
      .replace("<span class=\"tok-fn\">", "")
      .replace("</span>", "");
    let mut escaped = String::new();
    escape_into(&mut escaped, source);
    assert_eq!(stripped, escaped);
  }
}
