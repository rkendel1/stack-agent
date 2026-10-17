//! Strict, bounded syntax check for JSON carried as text inside the contract
//! (tool schemas, tool arguments, provider tool arguments, JSON tool output).
//!
//! It accepts exactly RFC 8259 JSON, rejects duplicate object keys (matching the
//! oracle's `std.json` behavior used by `ToolArgumentIntegrity.classifySerialized`),
//! rejects unpaired UTF-16 surrogate escapes, and enforces a nesting-depth limit. It
//! never builds a value tree, so its memory use is bounded by the depth limit and
//! the longest object key.

use std::collections::HashSet;

/// Kind of the root JSON value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonKind {
    /// `{...}`
    Object,
    /// `[...]`
    Array,
    /// A string.
    String,
    /// A number.
    Number,
    /// `true` or `false`.
    Bool,
    /// `null`.
    Null,
}

/// Why a JSON text was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonError {
    /// Not valid JSON.
    Syntax,
    /// An object repeats a key.
    DuplicateKey,
    /// Nesting exceeds the depth limit.
    TooDeep,
}

/// Checks `text` and returns the kind of its root value.
pub fn check(text: &str, max_depth: usize) -> Result<JsonKind, JsonError> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        pos: 0,
        max_depth,
    };
    parser.skip_ws();
    let kind = parser.value(0)?;
    parser.skip_ws();
    if parser.pos != parser.bytes.len() {
        return Err(JsonError::Syntax);
    }
    Ok(kind)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    max_depth: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }

    fn expect(&mut self, byte: u8) -> Result<(), JsonError> {
        if self.bump() == Some(byte) {
            Ok(())
        } else {
            Err(JsonError::Syntax)
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn value(&mut self, depth: usize) -> Result<JsonKind, JsonError> {
        match self.peek().ok_or(JsonError::Syntax)? {
            b'{' => self.object(depth + 1).map(|()| JsonKind::Object),
            b'[' => self.array(depth + 1).map(|()| JsonKind::Array),
            b'"' => self.string(None).map(|()| JsonKind::String),
            b't' => self.literal(b"true").map(|()| JsonKind::Bool),
            b'f' => self.literal(b"false").map(|()| JsonKind::Bool),
            b'n' => self.literal(b"null").map(|()| JsonKind::Null),
            b'-' | b'0'..=b'9' => self.number().map(|()| JsonKind::Number),
            _ => Err(JsonError::Syntax),
        }
    }

    fn literal(&mut self, word: &[u8]) -> Result<(), JsonError> {
        if self.bytes[self.pos..].starts_with(word) {
            self.pos += word.len();
            Ok(())
        } else {
            Err(JsonError::Syntax)
        }
    }

    fn object(&mut self, depth: usize) -> Result<(), JsonError> {
        if depth > self.max_depth {
            return Err(JsonError::TooDeep);
        }
        self.expect(b'{')?;
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(());
        }
        let mut keys: HashSet<String> = HashSet::new();
        loop {
            self.skip_ws();
            let mut key = String::new();
            self.string(Some(&mut key))?;
            if !keys.insert(key) {
                return Err(JsonError::DuplicateKey);
            }
            self.skip_ws();
            self.expect(b':')?;
            self.skip_ws();
            self.value(depth)?;
            self.skip_ws();
            match self.bump() {
                Some(b',') => continue,
                Some(b'}') => return Ok(()),
                _ => return Err(JsonError::Syntax),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<(), JsonError> {
        if depth > self.max_depth {
            return Err(JsonError::TooDeep);
        }
        self.expect(b'[')?;
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(());
        }
        loop {
            self.skip_ws();
            self.value(depth)?;
            self.skip_ws();
            match self.bump() {
                Some(b',') => continue,
                Some(b']') => return Ok(()),
                _ => return Err(JsonError::Syntax),
            }
        }
    }

    /// Parses a string; when `out` is given, decodes it (used for key comparison).
    fn string(&mut self, mut out: Option<&mut String>) -> Result<(), JsonError> {
        self.expect(b'"')?;
        let mut run_start = self.pos;
        loop {
            let byte = self.bump().ok_or(JsonError::Syntax)?;
            match byte {
                b'"' => {
                    if let Some(out) = out.as_deref_mut() {
                        out.push_str(self.run(run_start, self.pos - 1));
                    }
                    return Ok(());
                }
                b'\\' => {
                    if let Some(out) = out.as_deref_mut() {
                        out.push_str(self.run(run_start, self.pos - 1));
                    }
                    let decoded = self.escape()?;
                    if let Some(out) = out.as_deref_mut() {
                        out.push(decoded);
                    }
                    run_start = self.pos;
                }
                0x00..=0x1f => return Err(JsonError::Syntax),
                _ => {}
            }
        }
    }

    fn run(&self, start: usize, end: usize) -> &str {
        // The input is a &str and runs never split a UTF-8 sequence (they end at
        // ASCII '"' or '\\'), so this cannot fail.
        std::str::from_utf8(&self.bytes[start..end]).unwrap_or("")
    }

    fn escape(&mut self) -> Result<char, JsonError> {
        let decoded = match self.bump().ok_or(JsonError::Syntax)? {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let first = self.hex4()?;
                match first {
                    0xD800..=0xDBFF => {
                        if self.bump() != Some(b'\\') || self.bump() != Some(b'u') {
                            return Err(JsonError::Syntax);
                        }
                        let second = self.hex4()?;
                        if !(0xDC00..=0xDFFF).contains(&second) {
                            return Err(JsonError::Syntax);
                        }
                        let code = 0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00);
                        char::from_u32(code).ok_or(JsonError::Syntax)?
                    }
                    0xDC00..=0xDFFF => return Err(JsonError::Syntax),
                    _ => char::from_u32(first).ok_or(JsonError::Syntax)?,
                }
            }
            _ => return Err(JsonError::Syntax),
        };
        Ok(decoded)
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let mut value = 0u32;
        for _ in 0..4 {
            let digit = (self.bump().ok_or(JsonError::Syntax)? as char)
                .to_digit(16)
                .ok_or(JsonError::Syntax)?;
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn number(&mut self) -> Result<(), JsonError> {
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.bump() {
            Some(b'0') => {}
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(JsonError::Syntax),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            self.require_digits()?;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            self.require_digits()?;
        }
        Ok(())
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
    }

    fn require_digits(&mut self) -> Result<(), JsonError> {
        if !matches!(self.peek(), Some(b'0'..=b'9')) {
            return Err(JsonError::Syntax);
        }
        self.digits();
        Ok(())
    }
}
