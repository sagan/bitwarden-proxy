//! A streaming JSON rewriter.
//!
//! The rewriter works on the raw byte stream of an upstream response and never
//! buffers the whole document. Bytes are copied through verbatim; only the
//! values selected by a [`RuleSet`] are replaced. Input may arrive in
//! arbitrarily sized chunks (including in the middle of a key, literal or
//! string), which is required for HTTP bodies.

use std::sync::Arc;

use super::rules::{Action, RuleSet, Segment};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Object,
    Array,
}

/// One open JSON container.
///
/// For a value currently being parsed, the path is formed by walking the stack
/// and taking the key being processed in every object / the index being
/// processed in every array.
struct Frame {
    kind: FrameKind,
    key: Option<String>,
    index: usize,
}

impl Frame {
    fn object() -> Self {
        Frame {
            kind: FrameKind::Object,
            key: None,
            index: 0,
        }
    }

    fn array() -> Self {
        Frame {
            kind: FrameKind::Array,
            key: None,
            index: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Expecting a value (object value, array element or document root).
    Value,
    /// Object: expecting a key or `}`.
    ObjectKey,
    /// Object: expecting `:` after a key.
    ObjectColon,
    /// Object: expecting `,` or `}` after a value.
    ObjectComma,
    /// Array: expecting a value or `]`.
    ArrayValue,
    /// Array: expecting `,` or `]` after a value.
    ArrayComma,
    /// Inside an object key string.
    KeyString,
    /// Inside a string value.
    ValueString,
    /// Inside a number.
    Number,
    /// Inside `true`, `false` or `null`.
    Literal,
    /// The root value is complete; only whitespace is valid now.
    Done,
}

/// Incremental JSON rewriter.
///
/// Feed it chunks with [`JsonRewriter::push`]; the rewritten bytes are appended
/// to the supplied output buffer.
pub struct JsonRewriter {
    rules: Arc<RuleSet>,
    stack: Vec<Frame>,
    state: State,
    /// Raw bytes of the key / literal currently being scanned.
    token: Vec<u8>,
    escaped: bool,
    /// Index of the rule whose string replacement is being applied.
    replacement: Option<usize>,
    /// Set when the input stopped looking like JSON; the rest is copied as-is.
    passthrough: bool,
}

impl JsonRewriter {
    pub fn new(rules: Arc<RuleSet>) -> Self {
        Self {
            rules,
            stack: Vec::with_capacity(8),
            state: State::Value,
            token: Vec::new(),
            escaped: false,
            replacement: None,
            passthrough: false,
        }
    }

    /// Push a chunk of upstream bytes, appending rewritten output to `out`.
    pub fn push(&mut self, input: &[u8], out: &mut Vec<u8>) {
        if self.passthrough {
            out.extend_from_slice(input);
            return;
        }

        let mut i = 0;
        while i < input.len() {
            let byte = input[i];
            match self.state {
                State::Value => {
                    if is_whitespace(byte) {
                        out.push(byte);
                        i += 1;
                    } else if self.begin_value(byte, out) {
                        i += 1;
                    } else {
                        self.bail(out, input, i);
                        return;
                    }
                }
                State::ArrayValue => {
                    if is_whitespace(byte) {
                        out.push(byte);
                        i += 1;
                    } else if byte == b']' {
                        out.push(byte);
                        self.close_frame();
                        i += 1;
                    } else if self.begin_value(byte, out) {
                        i += 1;
                    } else {
                        self.bail(out, input, i);
                        return;
                    }
                }
                State::ObjectKey => {
                    if is_whitespace(byte) {
                        out.push(byte);
                        i += 1;
                    } else if byte == b'"' {
                        out.push(byte);
                        self.token.clear();
                        self.escaped = false;
                        self.state = State::KeyString;
                        i += 1;
                    } else if byte == b'}' {
                        out.push(byte);
                        self.close_frame();
                        i += 1;
                    } else {
                        self.bail(out, input, i);
                        return;
                    }
                }
                State::ObjectColon => {
                    if is_whitespace(byte) {
                        out.push(byte);
                        i += 1;
                    } else if byte == b':' {
                        out.push(byte);
                        self.state = State::Value;
                        i += 1;
                    } else {
                        self.bail(out, input, i);
                        return;
                    }
                }
                State::ObjectComma => {
                    if is_whitespace(byte) {
                        out.push(byte);
                        i += 1;
                    } else if byte == b',' {
                        out.push(byte);
                        if let Some(frame) = self.stack.last_mut() {
                            frame.key = None;
                        }
                        self.state = State::ObjectKey;
                        i += 1;
                    } else if byte == b'}' {
                        out.push(byte);
                        self.close_frame();
                        i += 1;
                    } else {
                        self.bail(out, input, i);
                        return;
                    }
                }
                State::ArrayComma => {
                    if is_whitespace(byte) {
                        out.push(byte);
                        i += 1;
                    } else if byte == b',' {
                        out.push(byte);
                        self.state = State::ArrayValue;
                        i += 1;
                    } else if byte == b']' {
                        out.push(byte);
                        self.close_frame();
                        i += 1;
                    } else {
                        self.bail(out, input, i);
                        return;
                    }
                }
                State::KeyString => {
                    if self.escaped {
                        self.escaped = false;
                        out.push(byte);
                        self.token.push(byte);
                        i += 1;
                    } else if byte == b'\\' {
                        self.escaped = true;
                        out.push(byte);
                        self.token.push(byte);
                        i += 1;
                    } else if byte == b'"' {
                        out.push(byte);
                        let key = decode_key(&self.token);
                        if let Some(frame) = self.stack.last_mut() {
                            frame.key = Some(key);
                        }
                        self.token.clear();
                        self.state = State::ObjectColon;
                        i += 1;
                    } else {
                        out.push(byte);
                        self.token.push(byte);
                        i += 1;
                    }
                }
                State::ValueString => {
                    if self.escaped {
                        self.escaped = false;
                        if self.replacement.is_none() {
                            out.push(byte);
                        }
                        i += 1;
                    } else if byte == b'\\' {
                        self.escaped = true;
                        if self.replacement.is_none() {
                            out.push(byte);
                        }
                        i += 1;
                    } else if byte == b'"' {
                        self.end_value_string(out);
                        i += 1;
                    } else {
                        if self.replacement.is_none() {
                            out.push(byte);
                        }
                        i += 1;
                    }
                }
                State::Number => {
                    if is_number_byte(byte) {
                        out.push(byte);
                        i += 1;
                    } else {
                        // Re-process the delimiter in the following state.
                        self.after_value();
                    }
                }
                State::Literal => {
                    if byte.is_ascii_alphabetic() && self.token.len() < 5 {
                        self.token.push(byte);
                        i += 1;
                    }

                    match self.token.as_slice() {
                        b"true" => {
                            self.emit_literal(true, out);
                            self.after_value();
                        }
                        b"false" => {
                            self.emit_literal(false, out);
                            self.after_value();
                        }
                        b"null" => {
                            out.extend_from_slice(b"null");
                            self.after_value();
                        }
                        token if is_literal_prefix(token) => {
                            // Need more input.
                        }
                        _ => {
                            self.bail(out, input, i);
                            return;
                        }
                    }
                }
                State::Done => {
                    if is_whitespace(byte) {
                        out.push(byte);
                        i += 1;
                    } else {
                        self.bail(out, input, i);
                        return;
                    }
                }
            }
        }
    }

    /// Finish the stream. All output is produced incrementally, so this only
    /// exists to make the end of the body explicit (and for symmetry).
    pub fn finish(&mut self, _out: &mut Vec<u8>) {}

    fn begin_value(&mut self, byte: u8, out: &mut Vec<u8>) -> bool {
        match byte {
            b'{' => {
                out.push(byte);
                self.stack.push(Frame::object());
                self.state = State::ObjectKey;
                true
            }
            b'[' => {
                out.push(byte);
                self.stack.push(Frame::array());
                self.state = State::ArrayValue;
                true
            }
            b'"' => {
                self.begin_value_string(out);
                true
            }
            b't' | b'f' | b'n' => {
                self.token.clear();
                self.token.push(byte);
                self.state = State::Literal;
                true
            }
            b'-' | b'0'..=b'9' => {
                out.push(byte);
                self.state = State::Number;
                true
            }
            _ => false,
        }
    }

    fn begin_value_string(&mut self, out: &mut Vec<u8>) {
        match self.find_action(|action| matches!(action, Action::SetString(_))) {
            Some(index) => {
                self.replacement = Some(index);
            }
            None => {
                self.replacement = None;
                out.push(b'"');
            }
        }
        self.escaped = false;
        self.state = State::ValueString;
    }

    fn end_value_string(&mut self, out: &mut Vec<u8>) {
        match self.replacement.take() {
            Some(index) => {
                if let Action::SetString(value) = &self.rules.rules()[index].action {
                    out.push(b'"');
                    escape_into(value, out);
                    out.push(b'"');
                }
            }
            None => out.push(b'"'),
        }
        self.after_value();
    }

    fn emit_literal(&mut self, value: bool, out: &mut Vec<u8>) {
        match self.find_action(|action| matches!(action, Action::SetBool(_))) {
            Some(index) => {
                if let Action::SetBool(replacement) = &self.rules.rules()[index].action {
                    out.extend_from_slice(if *replacement { b"true" } else { b"false" });
                    return;
                }
                out.extend_from_slice(if value { b"true" } else { b"false" });
            }
            None => out.extend_from_slice(if value { b"true" } else { b"false" }),
        }
    }

    /// Find a rule matching the current path whose action satisfies `filter`.
    fn find_action(&self, filter: impl Fn(&Action) -> bool) -> Option<usize> {
        let depth = self.stack.len();
        for &index in self.rules.candidates(depth) {
            let rule = &self.rules.rules()[index];
            if filter(&rule.action) && path_matches(&rule.path, &self.stack) {
                return Some(index);
            }
        }
        None
    }

    fn after_value(&mut self) {
        match self.stack.last_mut() {
            None => self.state = State::Done,
            Some(frame) => match frame.kind {
                FrameKind::Array => {
                    frame.index += 1;
                    self.state = State::ArrayComma;
                }
                FrameKind::Object => self.state = State::ObjectComma,
            },
        }
    }

    fn close_frame(&mut self) {
        self.stack.pop();
        self.after_value();
    }

    /// Stop parsing and copy the remainder of the input unchanged. The upstream
    /// is expected to produce JSON, but if it does not we degrade gracefully
    /// instead of corrupting the stream further.
    fn bail(&mut self, out: &mut Vec<u8>, input: &[u8], from: usize) {
        self.passthrough = true;
        // Literals are buffered until they are complete; emit what was
        // collected so the remainder is not shifted.
        if self.state == State::Literal {
            out.extend_from_slice(&self.token);
        }
        out.extend_from_slice(&input[from..]);
    }
}

fn path_matches(rule: &[Segment], stack: &[Frame]) -> bool {
    if rule.len() != stack.len() {
        return false;
    }
    rule.iter()
        .zip(stack)
        .all(|(segment, frame)| match segment {
            Segment::AnyIndex => frame.kind == FrameKind::Array,
            Segment::Key(key) => {
                frame.kind == FrameKind::Object && frame.key.as_deref() == Some(key.as_str())
            }
        })
}

fn is_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

fn is_number_byte(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E')
}

fn is_literal_prefix(token: &[u8]) -> bool {
    !token.is_empty()
        && (b"true".starts_with(token) || b"false".starts_with(token) || b"null".starts_with(token))
}

fn escape_into(value: &str, out: &mut Vec<u8>) {
    for &byte in value.as_bytes() {
        match byte {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x00..=0x1f => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                out.extend_from_slice(b"\\u00");
                out.push(HEX[(byte >> 4) as usize]);
                out.push(HEX[(byte & 0x0f) as usize]);
            }
            _ => out.push(byte),
        }
    }
}

fn decode_key(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    if !text.contains('\\') {
        return text.into_owned();
    }

    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('/') => out.push('/'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('u') => {
                let Some(high) = read_hex4(&mut chars) else {
                    return out;
                };
                if (0xd800..0xdc00).contains(&high) {
                    // Surrogate pair: expect `\uXXXX` with a low surrogate.
                    if chars.next() == Some('\\') && chars.next() == Some('u') {
                        if let Some(low) = read_hex4(&mut chars) {
                            if (0xdc00..0xe000).contains(&low) {
                                let combined = 0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00);
                                out.push(char::from_u32(combined).unwrap_or('\u{fffd}'));
                                continue;
                            }
                        }
                    }
                    out.push('\u{fffd}');
                } else {
                    out.push(char::from_u32(high).unwrap_or('\u{fffd}'));
                }
            }
            Some(other) => out.push(other),
            None => break,
        }
    }
    out
}

fn read_hex4(chars: &mut std::str::Chars<'_>) -> Option<u32> {
    let mut value = 0u32;
    for _ in 0..4 {
        value = value * 16 + chars.next()?.to_digit(16)?;
    }
    Some(value)
}
