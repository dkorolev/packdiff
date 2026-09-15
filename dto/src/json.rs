//! A small, dependency-free JSON codec: the one serialization layer every
//! packdiff document goes through, natively and in the browser build.
//!
//! [`Value`] is the generic tree ([`parse`] / [`Value::to_string`] /
//! [`Value::to_string_pretty`]); [`ToJson`] and [`FromJson`] are the typed
//! layer the data model implements by hand — every struct lists its fields
//! once for writing and once for reading, and reading is strict: an unknown
//! field, a missing required field, or a value of the wrong type is a hard
//! error, never a silently ignored key.
//!
//! Wire-format decisions, all deliberate and all pinned by tests:
//!
//! - Objects keep insertion order on write and source order on read, so a
//!   typed document serializes in its declared field order. [`Value::sorted`]
//!   gives the key-sorted form for the one place bytes must be
//!   order-independent (the page's content fingerprint).
//! - Compact output has no whitespace; pretty output uses two-space
//!   indentation, `"key": value`, one array element per line, and `[]` /
//!   `{}` for empties.
//! - Strings escape `"`, `\`, and the control characters (`\b` `\f` `\n`
//!   `\r` `\t`, `\u00XX` otherwise) and nothing else; non-ASCII text is
//!   written verbatim.
//! - Numbers are read as unsigned, signed, or floating point by their
//!   lexical form; typed fields accept only what fits them.
//! - Duplicate keys, trailing content, raw control characters inside
//!   strings, and nesting deeper than [`MAX_DEPTH`] are rejected.

use std::collections::BTreeMap;
use std::fmt;

/// Nesting depth beyond which a document is rejected, so hostile input
/// cannot exhaust the stack.
pub const MAX_DEPTH: usize = 128;

/// A codec failure: what was wrong and, for parse errors, where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
  message: String,
}

impl Error {
  fn new(message: impl Into<String>) -> Self {
    Self { message: message.into() }
  }
}

impl fmt::Display for Error {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.message)
  }
}

impl std::error::Error for Error {}

/// The codec's result type.
pub type Result<T> = std::result::Result<T, Error>;

/// A JSON number, kept in the widest lexical class it was written in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Number {
  /// A non-negative integer.
  PosInt(u64),
  /// A negative integer.
  NegInt(i64),
  /// Anything with a fraction or exponent, or an integer too wide for the
  /// two forms above.
  Float(f64),
}

/// An ordered JSON object: insertion order on write, source order on read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Map {
  entries: Vec<(String, Value)>,
}

impl Map {
  pub fn new() -> Self {
    Self::default()
  }

  /// Append or replace the entry for `key`.
  pub fn insert(&mut self, key: impl Into<String>, value: impl Into<Value>) {
    let key = key.into();
    let value = value.into();
    match self.entries.iter_mut().find(|(k, _)| *k == key) {
      Some(entry) => entry.1 = value,
      None => self.entries.push((key, value)),
    }
  }

  pub fn get(&self, key: &str) -> Option<&Value> {
    self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
  }

  pub fn contains_key(&self, key: &str) -> bool {
    self.get(key).is_some()
  }

  pub fn keys(&self) -> impl Iterator<Item = &str> {
    self.entries.iter().map(|(k, _)| k.as_str())
  }

  pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
    self.entries.iter().map(|(k, v)| (k.as_str(), v))
  }

  pub fn len(&self) -> usize {
    self.entries.len()
  }

  pub fn is_empty(&self) -> bool {
    self.entries.is_empty()
  }
}

impl<K: Into<String>, V: Into<Value>> FromIterator<(K, V)> for Map {
  fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
    let mut map = Map::new();
    for (k, v) in iter {
      map.insert(k, v);
    }
    map
  }
}

/// A JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
  Null,
  Bool(bool),
  Number(Number),
  String(String),
  Array(Vec<Value>),
  Object(Map),
}

static NULL: Value = Value::Null;

impl Value {
  /// An object from `(key, value)` pairs, in the given order.
  pub fn object<K: Into<String>, V: Into<Value>>(entries: impl IntoIterator<Item = (K, V)>) -> Value {
    Value::Object(entries.into_iter().collect())
  }

  /// An array from values.
  pub fn array<V: Into<Value>>(items: impl IntoIterator<Item = V>) -> Value {
    Value::Array(items.into_iter().map(Into::into).collect())
  }

  /// The member `key` of an object; `None` for anything else or a missing key.
  pub fn get(&self, key: &str) -> Option<&Value> {
    match self {
      Value::Object(map) => map.get(key),
      _ => None,
    }
  }

  pub fn is_null(&self) -> bool {
    matches!(self, Value::Null)
  }

  pub fn is_string(&self) -> bool {
    matches!(self, Value::String(_))
  }

  pub fn is_u64(&self) -> bool {
    matches!(self, Value::Number(Number::PosInt(_)))
  }

  pub fn as_bool(&self) -> Option<bool> {
    match self {
      Value::Bool(b) => Some(*b),
      _ => None,
    }
  }

  pub fn as_str(&self) -> Option<&str> {
    match self {
      Value::String(s) => Some(s),
      _ => None,
    }
  }

  pub fn as_u64(&self) -> Option<u64> {
    match self {
      Value::Number(Number::PosInt(n)) => Some(*n),
      _ => None,
    }
  }

  pub fn as_i64(&self) -> Option<i64> {
    match self {
      Value::Number(Number::PosInt(n)) => i64::try_from(*n).ok(),
      Value::Number(Number::NegInt(n)) => Some(*n),
      _ => None,
    }
  }

  pub fn as_array(&self) -> Option<&Vec<Value>> {
    match self {
      Value::Array(items) => Some(items),
      _ => None,
    }
  }

  pub fn as_object(&self) -> Option<&Map> {
    match self {
      Value::Object(map) => Some(map),
      _ => None,
    }
  }

  pub fn as_object_mut(&mut self) -> Option<&mut Map> {
    match self {
      Value::Object(map) => Some(map),
      _ => None,
    }
  }

  /// The same value with every object's keys in byte order, recursively —
  /// the order-independent form for content fingerprints.
  pub fn sorted(&self) -> Value {
    match self {
      Value::Array(items) => Value::Array(items.iter().map(Value::sorted).collect()),
      Value::Object(map) => {
        let sorted: BTreeMap<&str, Value> = map.iter().map(|(k, v)| (k, v.sorted())).collect();
        Value::Object(sorted.into_iter().collect())
      }
      other => other.clone(),
    }
  }

  /// Compact text: no whitespace.
  #[allow(clippy::inherent_to_string_shadow_display)]
  pub fn to_string(&self) -> String {
    let mut out = String::new();
    write_value(&mut out, self, None, 0);
    out
  }

  /// Pretty text: two-space indentation, one element per line.
  pub fn to_string_pretty(&self) -> String {
    let mut out = String::new();
    write_value(&mut out, self, Some("  "), 0);
    out
  }
}

impl fmt::Display for Value {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&Value::to_string(self))
  }
}

impl std::ops::Index<&str> for Value {
  type Output = Value;
  /// The member `key`, or `Null` when absent or not an object — so lookups
  /// chain (`doc["a"]["b"]`) without unwrapping at every step.
  fn index(&self, key: &str) -> &Value {
    self.get(key).unwrap_or(&NULL)
  }
}

impl std::ops::Index<usize> for Value {
  type Output = Value;
  fn index(&self, index: usize) -> &Value {
    self.as_array().and_then(|items| items.get(index)).unwrap_or(&NULL)
  }
}

impl PartialEq<str> for Value {
  fn eq(&self, other: &str) -> bool {
    self.as_str() == Some(other)
  }
}

impl PartialEq<&str> for Value {
  fn eq(&self, other: &&str) -> bool {
    self.as_str() == Some(*other)
  }
}

impl PartialEq<String> for Value {
  fn eq(&self, other: &String) -> bool {
    self.as_str() == Some(other.as_str())
  }
}

impl PartialEq<bool> for Value {
  fn eq(&self, other: &bool) -> bool {
    self.as_bool() == Some(*other)
  }
}

macro_rules! eq_integer {
  ($($t:ty),*) => {$(
    impl PartialEq<$t> for Value {
      fn eq(&self, other: &$t) -> bool {
        (*other as i128) == match self {
          Value::Number(Number::PosInt(n)) => i128::from(*n),
          Value::Number(Number::NegInt(n)) => i128::from(*n),
          _ => return false,
        }
      }
    }
  )*};
}
eq_integer!(u8, u16, u32, u64, usize, i8, i16, i32, i64);

impl From<bool> for Value {
  fn from(b: bool) -> Value {
    Value::Bool(b)
  }
}

impl From<&str> for Value {
  fn from(s: &str) -> Value {
    Value::String(s.to_string())
  }
}

impl From<String> for Value {
  fn from(s: String) -> Value {
    Value::String(s)
  }
}

impl From<&String> for Value {
  fn from(s: &String) -> Value {
    Value::String(s.clone())
  }
}

macro_rules! from_unsigned {
  ($($t:ty),*) => {$(
    impl From<$t> for Value {
      fn from(n: $t) -> Value {
        Value::Number(Number::PosInt(n as u64))
      }
    }
  )*};
}
from_unsigned!(u8, u16, u32, u64, usize);

macro_rules! from_signed {
  ($($t:ty),*) => {$(
    impl From<$t> for Value {
      fn from(n: $t) -> Value {
        if n < 0 { Value::Number(Number::NegInt(n as i64)) } else { Value::Number(Number::PosInt(n as u64)) }
      }
    }
  )*};
}
from_signed!(i8, i16, i32, i64);

impl<T: Into<Value>> From<Option<T>> for Value {
  fn from(o: Option<T>) -> Value {
    o.map_or(Value::Null, Into::into)
  }
}

impl<T: Into<Value>> From<Vec<T>> for Value {
  fn from(items: Vec<T>) -> Value {
    Value::Array(items.into_iter().map(Into::into).collect())
  }
}

impl From<Map> for Value {
  fn from(map: Map) -> Value {
    Value::Object(map)
  }
}

// ------------------------------------------------------------------ writer

fn write_value(out: &mut String, value: &Value, indent: Option<&str>, depth: usize) {
  match value {
    Value::Null => out.push_str("null"),
    Value::Bool(true) => out.push_str("true"),
    Value::Bool(false) => out.push_str("false"),
    Value::Number(Number::PosInt(n)) => out.push_str(&n.to_string()),
    Value::Number(Number::NegInt(n)) => out.push_str(&n.to_string()),
    Value::Number(Number::Float(f)) => {
      if f.is_finite() {
        // Shortest round-trip form, with the conventional explicit `+` on
        // a positive exponent (`1e+21`).
        out.push_str(&format!("{f:?}").replace('e', "e+").replace("e+-", "e-"));
      } else {
        out.push_str("null");
      }
    }
    Value::String(s) => write_string(out, s),
    Value::Array(items) => {
      if items.is_empty() {
        out.push_str("[]");
        return;
      }
      out.push('[');
      for (i, item) in items.iter().enumerate() {
        if i > 0 {
          out.push(',');
        }
        newline(out, indent, depth + 1);
        write_value(out, item, indent, depth + 1);
      }
      newline(out, indent, depth);
      out.push(']');
    }
    Value::Object(map) => {
      if map.is_empty() {
        out.push_str("{}");
        return;
      }
      out.push('{');
      for (i, (key, item)) in map.iter().enumerate() {
        if i > 0 {
          out.push(',');
        }
        newline(out, indent, depth + 1);
        write_string(out, key);
        out.push(':');
        if indent.is_some() {
          out.push(' ');
        }
        write_value(out, item, indent, depth + 1);
      }
      newline(out, indent, depth);
      out.push('}');
    }
  }
}

fn newline(out: &mut String, indent: Option<&str>, depth: usize) {
  if let Some(unit) = indent {
    out.push('\n');
    for _ in 0..depth {
      out.push_str(unit);
    }
  }
}

fn write_string(out: &mut String, s: &str) {
  out.push('"');
  for ch in s.chars() {
    match ch {
      '"' => out.push_str("\\\""),
      '\\' => out.push_str("\\\\"),
      '\u{8}' => out.push_str("\\b"),
      '\u{c}' => out.push_str("\\f"),
      '\n' => out.push_str("\\n"),
      '\r' => out.push_str("\\r"),
      '\t' => out.push_str("\\t"),
      c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
      c => out.push(c),
    }
  }
  out.push('"');
}

// ------------------------------------------------------------------ parser

/// Parse one JSON document. Trailing non-whitespace is an error.
pub fn parse(text: &str) -> Result<Value> {
  let mut p = Parser { bytes: text.as_bytes(), pos: 0 };
  p.skip_ws();
  let value = p.value(0)?;
  p.skip_ws();
  if p.pos < p.bytes.len() {
    return Err(p.error("trailing characters"));
  }
  Ok(value)
}

struct Parser<'a> {
  bytes: &'a [u8],
  pos: usize,
}

impl Parser<'_> {
  fn error(&self, what: &str) -> Error {
    let (mut line, mut column) = (1, 0);
    for &b in &self.bytes[..self.pos.min(self.bytes.len())] {
      if b == b'\n' {
        line += 1;
        column = 0;
      } else {
        column += 1;
      }
    }
    Error::new(format!("{what} at line {line} column {column}"))
  }

  fn skip_ws(&mut self) {
    while let Some(&b) = self.bytes.get(self.pos) {
      if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
        self.pos += 1;
      } else {
        break;
      }
    }
  }

  fn peek(&self) -> Option<u8> {
    self.bytes.get(self.pos).copied()
  }

  fn expect_literal(&mut self, literal: &str, value: Value) -> Result<Value> {
    if self.bytes[self.pos..].starts_with(literal.as_bytes()) {
      self.pos += literal.len();
      Ok(value)
    } else {
      Err(self.error("expected value"))
    }
  }

  fn value(&mut self, depth: usize) -> Result<Value> {
    if depth > MAX_DEPTH {
      return Err(self.error("recursion limit exceeded"));
    }
    match self.peek() {
      None => Err(self.error("EOF while parsing a value")),
      Some(b'n') => self.expect_literal("null", Value::Null),
      Some(b't') => self.expect_literal("true", Value::Bool(true)),
      Some(b'f') => self.expect_literal("false", Value::Bool(false)),
      Some(b'"') => Ok(Value::String(self.string()?)),
      Some(b'[') => self.array(depth),
      Some(b'{') => self.object(depth),
      Some(b'-' | b'0'..=b'9') => self.number(),
      Some(_) => Err(self.error("expected value")),
    }
  }

  fn array(&mut self, depth: usize) -> Result<Value> {
    self.pos += 1; // [
    let mut items = Vec::new();
    self.skip_ws();
    if self.peek() == Some(b']') {
      self.pos += 1;
      return Ok(Value::Array(items));
    }
    loop {
      self.skip_ws();
      items.push(self.value(depth + 1)?);
      self.skip_ws();
      match self.peek() {
        Some(b',') => self.pos += 1,
        Some(b']') => {
          self.pos += 1;
          return Ok(Value::Array(items));
        }
        Some(_) => return Err(self.error("expected `,` or `]`")),
        None => return Err(self.error("EOF while parsing a list")),
      }
    }
  }

  fn object(&mut self, depth: usize) -> Result<Value> {
    self.pos += 1; // {
    let mut map = Map::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    self.skip_ws();
    if self.peek() == Some(b'}') {
      self.pos += 1;
      return Ok(Value::Object(map));
    }
    loop {
      self.skip_ws();
      if self.peek() != Some(b'"') {
        return Err(self.error("key must be a string"));
      }
      let key = self.string()?;
      if !seen.insert(key.clone()) {
        return Err(self.error(&format!("duplicate field `{key}`")));
      }
      self.skip_ws();
      if self.peek() != Some(b':') {
        return Err(self.error("expected `:`"));
      }
      self.pos += 1;
      self.skip_ws();
      let value = self.value(depth + 1)?;
      map.entries.push((key, value));
      self.skip_ws();
      match self.peek() {
        Some(b',') => self.pos += 1,
        Some(b'}') => {
          self.pos += 1;
          return Ok(Value::Object(map));
        }
        Some(_) => return Err(self.error("expected `,` or `}`")),
        None => return Err(self.error("EOF while parsing an object")),
      }
    }
  }

  fn number(&mut self) -> Result<Value> {
    let start = self.pos;
    let negative = self.peek() == Some(b'-');
    if negative {
      self.pos += 1;
    }
    let int_start = self.pos;
    match self.peek() {
      Some(b'0') => self.pos += 1,
      Some(b'1'..=b'9') => {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
          self.pos += 1;
        }
      }
      _ => return Err(self.error("invalid number")),
    }
    let mut integral = true;
    if self.peek() == Some(b'.') {
      integral = false;
      self.pos += 1;
      if !matches!(self.peek(), Some(b'0'..=b'9')) {
        return Err(self.error("invalid number"));
      }
      while matches!(self.peek(), Some(b'0'..=b'9')) {
        self.pos += 1;
      }
    }
    if matches!(self.peek(), Some(b'e' | b'E')) {
      integral = false;
      self.pos += 1;
      if matches!(self.peek(), Some(b'+' | b'-')) {
        self.pos += 1;
      }
      if !matches!(self.peek(), Some(b'0'..=b'9')) {
        return Err(self.error("invalid number"));
      }
      while matches!(self.peek(), Some(b'0'..=b'9')) {
        self.pos += 1;
      }
    }
    // The slice is ASCII digits and punctuation by construction.
    let text = std::str::from_utf8(&self.bytes[start..self.pos]).expect("number text is ASCII");
    let digits = std::str::from_utf8(&self.bytes[int_start..self.pos]).expect("number text is ASCII");
    if integral {
      if negative {
        if let Ok(n) = text.parse::<i64>() {
          return Ok(Value::Number(NegInt(n)));
        }
      } else if let Ok(n) = digits.parse::<u64>() {
        return Ok(Value::Number(Number::PosInt(n)));
      }
    }
    text.parse::<f64>().map(|f| Value::Number(Number::Float(f))).map_err(|_| self.error("invalid number"))
  }

  fn string(&mut self) -> Result<String> {
    self.pos += 1; // opening quote
    let mut out = String::new();
    loop {
      let Some(b) = self.peek() else {
        return Err(self.error("EOF while parsing a string"));
      };
      match b {
        b'"' => {
          self.pos += 1;
          return Ok(out);
        }
        b'\\' => {
          self.pos += 1;
          let Some(esc) = self.peek() else {
            return Err(self.error("EOF while parsing a string"));
          };
          self.pos += 1;
          match esc {
            b'"' => out.push('"'),
            b'\\' => out.push('\\'),
            b'/' => out.push('/'),
            b'b' => out.push('\u{8}'),
            b'f' => out.push('\u{c}'),
            b'n' => out.push('\n'),
            b'r' => out.push('\r'),
            b't' => out.push('\t'),
            b'u' => {
              let unit = self.hex4()?;
              let ch = if (0xD800..0xDC00).contains(&unit) {
                // A leading surrogate must be followed by `\uDC00`–`\uDFFF`.
                if self.bytes[self.pos..].starts_with(b"\\u") {
                  self.pos += 2;
                  let low = self.hex4()?;
                  if !(0xDC00..0xE000).contains(&low) {
                    return Err(self.error("lone leading surrogate in hex escape"));
                  }
                  char::from_u32(0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00))
                } else {
                  return Err(self.error("lone leading surrogate in hex escape"));
                }
              } else {
                char::from_u32(unit)
              };
              match ch {
                Some(c) => out.push(c),
                None => return Err(self.error("lone trailing surrogate in hex escape")),
              }
            }
            _ => return Err(self.error("invalid escape")),
          }
        }
        0x00..=0x1F => return Err(self.error("control character (U+0000–U+001F) found while parsing a string")),
        _ => {
          // Copy one UTF-8 scalar; the input is a `&str`, so boundaries are sound.
          let rest = std::str::from_utf8(&self.bytes[self.pos..]).expect("input is a str");
          let ch = rest.chars().next().expect("non-empty by the peek above");
          out.push(ch);
          self.pos += ch.len_utf8();
        }
      }
    }
  }

  fn hex4(&mut self) -> Result<u32> {
    let end = self.pos + 4;
    let Some(hex) = self.bytes.get(self.pos..end) else {
      return Err(self.error("EOF while parsing a string"));
    };
    let text = std::str::from_utf8(hex).map_err(|_| self.error("invalid escape"))?;
    let unit = u32::from_str_radix(text, 16).map_err(|_| self.error("invalid escape"))?;
    self.pos = end;
    Ok(unit)
  }
}

use Number::NegInt;

// ------------------------------------------------------------- typed layer

/// Write a typed value as JSON.
pub trait ToJson {
  fn to_json(&self) -> Value;
}

/// Read a typed value from JSON, strictly: wrong types, missing required
/// fields, and unknown fields are errors.
pub trait FromJson: Sized {
  fn from_json(value: &Value) -> Result<Self>;
}

/// Parse text straight into a typed value.
pub fn from_str<T: FromJson>(text: &str) -> Result<T> {
  T::from_json(&parse(text)?)
}

impl ToJson for Value {
  fn to_json(&self) -> Value {
    self.clone()
  }
}

impl FromJson for Value {
  fn from_json(value: &Value) -> Result<Self> {
    Ok(value.clone())
  }
}

impl ToJson for bool {
  fn to_json(&self) -> Value {
    Value::Bool(*self)
  }
}

impl FromJson for bool {
  fn from_json(value: &Value) -> Result<Self> {
    value.as_bool().ok_or_else(|| Error::new(format!("invalid type: expected a boolean, found {}", kind(value))))
  }
}

impl ToJson for str {
  fn to_json(&self) -> Value {
    Value::String(self.to_string())
  }
}

impl ToJson for String {
  fn to_json(&self) -> Value {
    Value::String(self.clone())
  }
}

impl FromJson for String {
  fn from_json(value: &Value) -> Result<Self> {
    value
      .as_str()
      .map(str::to_string)
      .ok_or_else(|| Error::new(format!("invalid type: expected a string, found {}", kind(value))))
  }
}

macro_rules! unsigned_json {
  ($($t:ty),*) => {$(
    impl ToJson for $t {
      fn to_json(&self) -> Value {
        Value::Number(Number::PosInt(*self as u64))
      }
    }
    impl FromJson for $t {
      fn from_json(value: &Value) -> Result<Self> {
        match value {
          Value::Number(Number::PosInt(n)) => <$t>::try_from(*n)
            .map_err(|_| Error::new(format!("invalid value: integer `{n}` does not fit {}", stringify!($t)))),
          other => Err(Error::new(format!("invalid type: expected an unsigned integer, found {}", kind(other)))),
        }
      }
    }
  )*};
}
unsigned_json!(u8, u16, u32, u64, usize);

impl<T: ToJson> ToJson for Option<T> {
  fn to_json(&self) -> Value {
    match self {
      Some(v) => v.to_json(),
      None => Value::Null,
    }
  }
}

impl<T: FromJson> FromJson for Option<T> {
  fn from_json(value: &Value) -> Result<Self> {
    match value {
      Value::Null => Ok(None),
      other => T::from_json(other).map(Some),
    }
  }
}

impl<T: ToJson> ToJson for Vec<T> {
  fn to_json(&self) -> Value {
    Value::Array(self.iter().map(ToJson::to_json).collect())
  }
}

impl<T: ToJson> ToJson for [T] {
  fn to_json(&self) -> Value {
    Value::Array(self.iter().map(ToJson::to_json).collect())
  }
}

impl<T: FromJson> FromJson for Vec<T> {
  fn from_json(value: &Value) -> Result<Self> {
    match value {
      Value::Array(items) => items.iter().map(T::from_json).collect(),
      other => Err(Error::new(format!("invalid type: expected an array, found {}", kind(other)))),
    }
  }
}

impl<T: ToJson> ToJson for BTreeMap<String, T> {
  fn to_json(&self) -> Value {
    Value::Object(self.iter().map(|(k, v)| (k.clone(), v.to_json())).collect())
  }
}

impl<T: FromJson> FromJson for BTreeMap<String, T> {
  fn from_json(value: &Value) -> Result<Self> {
    match value {
      Value::Object(map) => map.iter().map(|(k, v)| Ok((k.to_string(), T::from_json(v)?))).collect(),
      other => Err(Error::new(format!("invalid type: expected an object, found {}", kind(other)))),
    }
  }
}

/// The value's JSON type, for error messages.
fn kind(value: &Value) -> &'static str {
  match value {
    Value::Null => "null",
    Value::Bool(_) => "a boolean",
    Value::Number(_) => "a number",
    Value::String(_) => "a string",
    Value::Array(_) => "an array",
    Value::Object(_) => "an object",
  }
}

/// Field-by-field reader for a struct: every field is claimed once by
/// name, and [`Fields::finish`] rejects whatever was not claimed — the
/// strict "no unknown fields" rule, enforced at the one place a struct's
/// fields are listed for reading.
pub struct Fields<'a> {
  map: &'a Map,
  claimed: Vec<bool>,
  what: &'static str,
}

impl<'a> Fields<'a> {
  /// Start reading `what` (a type name, for messages) from an object.
  pub fn of(value: &'a Value, what: &'static str) -> Result<Self> {
    match value {
      Value::Object(map) => Ok(Self { map, claimed: vec![false; map.len()], what }),
      other => Err(Error::new(format!("invalid type: expected an object for {what}, found {}", kind(other)))),
    }
  }

  fn claim(&mut self, key: &str) -> Option<&'a Value> {
    let index = self.map.entries.iter().position(|(k, _)| k == key)?;
    self.claimed[index] = true;
    Some(&self.map.entries[index].1)
  }

  /// A field that must be present.
  pub fn required<T: FromJson>(&mut self, key: &str) -> Result<T> {
    match self.claim(key) {
      Some(v) => T::from_json(v).map_err(|e| Error::new(format!("{}.{key}: {e}", self.what))),
      None => Err(Error::new(format!("missing field `{key}` in {}", self.what))),
    }
  }

  /// A field that may be absent (or `null`), read as `None` then.
  pub fn optional<T: FromJson>(&mut self, key: &str) -> Result<Option<T>> {
    match self.claim(key) {
      Some(Value::Null) | None => Ok(None),
      Some(v) => T::from_json(v).map(Some).map_err(|e| Error::new(format!("{}.{key}: {e}", self.what))),
    }
  }

  /// A field that takes its type's default when absent.
  pub fn or_default<T: FromJson + Default>(&mut self, key: &str) -> Result<T> {
    match self.claim(key) {
      Some(v) => T::from_json(v).map_err(|e| Error::new(format!("{}.{key}: {e}", self.what))),
      None => Ok(T::default()),
    }
  }

  /// Every field has been claimed; anything left is unknown and rejected.
  pub fn finish(self) -> Result<()> {
    match self.map.entries.iter().zip(&self.claimed).find(|(_, claimed)| !**claimed) {
      Some(((key, _), _)) => Err(Error::new(format!("unknown field `{key}` in {}", self.what))),
      None => Ok(()),
    }
  }
}

/// Read a single-key union — `{ "Variant": payload }` — as its variant
/// name and payload.
pub fn union<'a>(value: &'a Value, what: &'static str) -> Result<(&'a str, &'a Value)> {
  match value {
    Value::Object(map) if map.len() == 1 => {
      let (name, payload) = map.iter().next().expect("one entry");
      Ok((name, payload))
    }
    Value::Object(_) => Err(Error::new(format!("expected a single-key union for {what}"))),
    other => Err(Error::new(format!("invalid type: expected an object for {what}, found {}", kind(other)))),
  }
}

/// Read a unit-variant enum written as its bare name.
pub fn variant_name<'a>(value: &'a Value, what: &'static str) -> Result<&'a str> {
  value
    .as_str()
    .ok_or_else(|| Error::new(format!("invalid type: expected a variant name for {what}, found {}", kind(value))))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_every_value_kind_and_keeps_object_order() {
    let v = parse(r#" { "z": [1, -2, 3.5, true, false, null, "s\n\"q\" \u00e9 \ud83d\ude00"], "a": {} } "#).unwrap();
    assert_eq!(v["z"][0], 1u64);
    assert_eq!(v["z"][1], -2);
    assert_eq!(v["z"][2], Value::Number(Number::Float(3.5)));
    assert_eq!(v["z"][3], true);
    assert!(v["z"][5].is_null());
    assert_eq!(v["z"][6], "s\n\"q\" é 😀");
    assert_eq!(v.as_object().unwrap().keys().collect::<Vec<_>>(), ["z", "a"], "source order kept");
    assert!(v["missing"]["deeper"].is_null(), "chained lookups never panic");
  }

  #[test]
  fn rejects_malformed_input() {
    for bad in [
      "",
      "nul",
      "[1,]",
      "{\"a\":1,}",
      "{a:1}",
      "{\"a\" 1}",
      "[1 2]",
      "\"open",
      "\"tab\there\"",
      "01",
      "1.",
      "-",
      "1e",
      "\"\\x\"",
      "\"\\ud83d\"",
      "{\"a\":1} x",
      "{\"a\":1,\"a\":2}",
    ] {
      assert!(parse(bad).is_err(), "{bad:?} must not parse");
    }
    let deep = "[".repeat(MAX_DEPTH + 2) + &"]".repeat(MAX_DEPTH + 2);
    assert!(parse(&deep).unwrap_err().to_string().contains("recursion limit"));
    assert!(parse("{\"a\":1,\"a\":2}").unwrap_err().to_string().contains("duplicate field `a`"));
    assert!(parse("[1,\n 2,\n x]").unwrap_err().to_string().contains("line 3 column 1"));
  }

  #[test]
  fn compact_and_pretty_match_the_conventional_layout() {
    let v = Value::object([
      ("b", Value::from(1u32)),
      ("a", Value::array(["x", "y"])),
      ("e", Value::array(Vec::<Value>::new())),
      ("o", Value::object(Vec::<(&str, Value)>::new())),
      ("n", Value::Null),
      ("s", Value::from("q\"\\\n\t\u{1}\u{7f}é")),
    ]);
    assert_eq!(
      v.to_string(),
      "{\"b\":1,\"a\":[\"x\",\"y\"],\"e\":[],\"o\":{},\"n\":null,\"s\":\"q\\\"\\\\\\n\\t\\u0001\u{7f}é\"}"
    );
    assert_eq!(
      v.to_string_pretty(),
      "{\n  \"b\": 1,\n  \"a\": [\n    \"x\",\n    \"y\"\n  ],\n  \"e\": [],\n  \"o\": {},\n  \"n\": null,\n  \"s\": \"q\\\"\\\\\\n\\t\\u0001\u{7f}é\"\n}"
    );
    assert_eq!(parse(&v.to_string()).unwrap(), v, "compact round-trips");
    assert_eq!(parse(&v.to_string_pretty()).unwrap(), v, "pretty round-trips");
  }

  #[test]
  fn sorted_orders_keys_recursively() {
    let v = parse(r#"{"b":{"d":1,"c":[{"z":1,"y":2}]},"a":0}"#).unwrap();
    assert_eq!(v.sorted().to_string(), r#"{"a":0,"b":{"c":[{"y":2,"z":1}],"d":1}}"#);
  }

  #[test]
  fn numbers_keep_their_lexical_class() {
    assert_eq!(parse("18446744073709551615").unwrap(), 18446744073709551615u64);
    assert_eq!(parse("-9223372036854775808").unwrap(), i64::MIN);
    assert!(matches!(parse("18446744073709551616").unwrap(), Value::Number(Number::Float(_))), "too wide → float");
    assert_eq!(parse("1e2").unwrap().to_string(), "100.0");
    assert_eq!(parse("[1e21,2.5e-7]").unwrap().to_string(), "[1e+21,2.5e-7]");
    assert_eq!(
      u32::from_json(&parse("4294967296").unwrap()).unwrap_err().to_string(),
      "invalid value: integer `4294967296` does not fit u32"
    );
    assert!(u64::from_json(&parse("-1").unwrap()).is_err());
    assert!(u64::from_json(&parse("1.0").unwrap()).is_err());
  }

  #[derive(Debug)]
  struct Point {
    x: u32,
    label: Option<String>,
    tags: Vec<String>,
  }

  impl FromJson for Point {
    fn from_json(value: &Value) -> Result<Self> {
      let mut f = Fields::of(value, "Point")?;
      let p = Point { x: f.required("x")?, label: f.optional("label")?, tags: f.or_default("tags")? };
      f.finish()?;
      Ok(p)
    }
  }

  #[test]
  fn fields_reader_is_strict() {
    let p: Point = from_str(r#"{"x": 1, "label": null}"#).unwrap();
    assert_eq!((p.x, p.label, p.tags.len()), (1, None, 0));
    let p: Point = from_str(r#"{"tags": ["t"], "label": "l", "x": 2}"#).unwrap();
    assert_eq!((p.x, p.label.as_deref(), p.tags.len()), (2, Some("l"), 1));
    let err = |text: &str| from_str::<Point>(text).unwrap_err().to_string();
    assert_eq!(err(r#"{"label": "l"}"#), "missing field `x` in Point");
    assert_eq!(err(r#"{"x": 1, "sneaky": true}"#), "unknown field `sneaky` in Point");
    assert_eq!(err(r#"{"x": "one"}"#), "Point.x: invalid type: expected an unsigned integer, found a string");
    assert_eq!(err("[]"), "invalid type: expected an object for Point, found an array");
  }

  #[test]
  fn unions_and_variant_names() {
    let add = parse(r#"{"Add": {"new": 1}}"#).unwrap();
    let (name, payload) = union(&add, "Line").unwrap();
    assert_eq!((name, payload["new"].as_u64()), ("Add", Some(1)));
    assert!(union(&parse(r#"{"Add": {}, "Del": {}}"#).unwrap(), "Line").is_err());
    assert_eq!(variant_name(&Value::from("Old"), "Side").unwrap(), "Old");
    assert!(variant_name(&Value::from(1u8), "Side").is_err());
  }
}

// Byte-for-byte parity with serde_json, checked while serde_json is still a
// dependency of this crate; the block goes with that dependency.
#[cfg(test)]
mod serde_parity {
  use super::*;

  fn samples() -> Vec<String> {
    vec![
      concat!(
        r#"{"schema_version":3,"tool":"packdiff","repo":"r","base":{"name":"main","sha":"aaaa"},"#,
        r#""files":[{"old_path":null,"new_path":"a/b.rs","status":"Added","binary":false,"hunks":[{"header":"@@ -0,0 +1,2 @@","#,
        r#""lines":[{"Add":{"new":1,"text":"fn x() { \"q\" <b> \\ \t é 😀 \u0001 \u007f }"}},{"Meta":{"text":"\\ No newline"}}]}],"#,
        r#""additions":2,"deletions":0,"notes":[]}],"empty":{},"n":[1,-2,18446744073709551615,-9223372036854775808],"f":[1.5,1e2,2.5e-7,1e21]}"#
      )
      .to_string(),
      "[]".to_string(),
      "{}".to_string(),
      "\"s\"".to_string(),
      "0".to_string(),
      "null".to_string(),
    ]
  }

  #[test]
  fn compact_pretty_and_sorted_output_match_serde_json() {
    for text in samples() {
      let ours = parse(&text).unwrap();
      let theirs: serde_json::Value = serde_json::from_str(&text).unwrap();
      assert_eq!(ours.sorted().to_string(), serde_json::to_string(&theirs).unwrap(), "compact (sorted) for {text}");
      assert_eq!(ours.sorted().to_string_pretty(), serde_json::to_string_pretty(&theirs).unwrap(), "pretty for {text}");
    }
  }
}
