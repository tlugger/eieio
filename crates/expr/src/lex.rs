//! The lexer (EXPR-SPEC §3.1).
//!
//! Tokens are produced one at a time from a byte cursor over the source. The
//! grammar is small enough that there is no table and no regex — and must stay so:
//! this compiles into the MCU leaf runtime.

use alloc::string::String;

use eio_signal::Value;

use crate::error::Error;
use crate::span::Span;

/// One token, with the span it came from.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Token {
    pub(crate) kind: TokenKind,
    pub(crate) span: Span,
}

/// The token kinds of EXPR §3.1.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TokenKind {
    /// `(`
    Open,
    /// `)`
    Close,
    /// A number or string literal, or one of the reserved symbols `true`, `false`,
    /// `null`, which EXPR §3.1 says evaluate to themselves — so the lexer resolves
    /// them here rather than leaving three special symbols for the parser.
    Literal(Value),
    /// An identifier.
    Symbol(String),
    /// `$` — the whole signal (EXPR §6).
    Signal,
    /// `$name` — single-level signal access (EXPR §6).
    Attr(String),
}

/// Whether `b` may start a symbol: `symstart` in EXPR §3.1.
///
/// `letter` is ASCII alphabetic. The spec leaves `letter` undefined; EXPR §7.4
/// already commits to ASCII-only case mapping for "`no_std` locale honesty", and
/// Unicode identifier classification needs tables that would not fit the MCU
/// budget.
fn is_symbol_start(b: u8) -> bool {
    b.is_ascii_alphabetic()
        || matches!(
            b,
            b'+' | b'-' | b'*' | b'/' | b'=' | b'<' | b'>' | b'!' | b'?' | b'_'
        )
}

/// Whether `b` may continue a symbol: `symchar` in EXPR §3.1.
fn is_symbol_char(b: u8) -> bool {
    is_symbol_start(b) || b.is_ascii_digit() || b == b'.'
}

/// Whitespace per EXPR §3.1: space, tab, newline. Carriage return is included so
/// that CRLF sources lex, which costs nothing and avoids a baffling failure on a
/// file edited on Windows.
fn is_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// Turns source text into tokens.
pub(crate) struct Lexer<'a> {
    source: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub(crate) fn new(source: &'a str) -> Self {
        Self {
            source,
            bytes: source.as_bytes(),
            pos: 0,
        }
    }

    /// The current byte offset.
    pub(crate) fn offset(&self) -> u32 {
        self.pos as u32
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<u8> {
        self.bytes.get(self.pos + ahead).copied()
    }

    /// Skips whitespace and comments. A comment runs from `;` to end of line
    /// (EXPR §3.1); one at end of input without a trailing newline is fine.
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(b) if is_whitespace(b) => self.pos += 1,
                Some(b';') => {
                    while let Some(b) = self.peek() {
                        if b == b'\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => return,
            }
        }
    }

    /// Produces the next token, or `None` at end of input.
    pub(crate) fn next_token(&mut self) -> Result<Option<Token>, Error> {
        self.skip_trivia();
        let start = self.pos as u32;
        let Some(b) = self.peek() else {
            return Ok(None);
        };

        let kind = match b {
            b'(' => {
                self.pos += 1;
                TokenKind::Open
            }
            b')' => {
                self.pos += 1;
                TokenKind::Close
            }
            b'"' => self.lex_string()?,
            b'$' => self.lex_sigil(),
            // `-` is both a `symstart` and the number sign, so EXPR §3.1 is
            // ambiguous as written. The tie-break: `-` starts a number only when a
            // digit follows it. So `-5` is a number while `-` and `-foo` are
            // symbols — which is what makes `(- 1 2)` and `(- 1)` lex as intended.
            b'0'..=b'9' => self.lex_number()?,
            b'-' if self.peek_at(1).is_some_and(|b| b.is_ascii_digit()) => self.lex_number()?,
            b if is_symbol_start(b) => self.lex_symbol(),
            _ => {
                // Advance past the whole character, not one byte, so the span
                // covers a multi-byte character and stays on a boundary.
                self.pos += self.char_len();
                return Err(Error::parse(
                    Span::new(start, self.pos as u32),
                    "unexpected character",
                ));
            }
        };

        Ok(Some(Token {
            kind,
            span: Span::new(start, self.pos as u32),
        }))
    }

    /// Byte length of the UTF-8 character at the cursor.
    fn char_len(&self) -> usize {
        self.source[self.pos..]
            .chars()
            .next()
            .map_or(1, char::len_utf8)
    }

    /// `symbol := symstart symchar*`, resolving the three reserved symbols.
    fn lex_symbol(&mut self) -> TokenKind {
        let start = self.pos;
        while self.peek().is_some_and(is_symbol_char) {
            self.pos += 1;
        }
        let text = &self.source[start..self.pos];
        match text {
            // EXPR §3.1: `true`, `false` and `null` are reserved symbols evaluating
            // to themselves. Resolving them here means the parser sees a literal,
            // which is also what makes the §5.2 shadowing check fall out: a `let`
            // binding name that is one of these arrives as a literal, not a symbol.
            "true" => TokenKind::Literal(Value::Bool(true)),
            "false" => TokenKind::Literal(Value::Bool(false)),
            "null" => TokenKind::Literal(Value::Null),
            _ => TokenKind::Symbol(String::from(text)),
        }
    }

    /// `sigil := "$" [symbol]` (EXPR §3.1, §6).
    fn lex_sigil(&mut self) -> TokenKind {
        self.pos += 1; // the `$`
        let start = self.pos;
        while self.peek().is_some_and(is_symbol_char) {
            self.pos += 1;
        }
        if start == self.pos {
            TokenKind::Signal
        } else {
            TokenKind::Attr(String::from(&self.source[start..self.pos]))
        }
    }

    /// `number := int | float` (EXPR §3.1).
    ///
    /// `int := ["-"] digit+`; a float needs either a fractional part with digits on
    /// both sides of the point, or an exponent. So `1.` and `.5` are not numbers,
    /// which keeps `.` unambiguous as a `symchar`.
    fn lex_number(&mut self) -> Result<TokenKind, Error> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        self.take_digits();

        let mut is_float = false;
        // A `.` is only part of the number when digits follow it; otherwise it
        // belongs to whatever comes next and the number ends here.
        if self.peek() == Some(b'.') && self.peek_at(1).is_some_and(|b| b.is_ascii_digit()) {
            is_float = true;
            self.pos += 1;
            self.take_digits();
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            let after_sign = if matches!(self.peek_at(1), Some(b'+' | b'-')) {
                2
            } else {
                1
            };
            if self.peek_at(after_sign).is_some_and(|b| b.is_ascii_digit()) {
                is_float = true;
                self.pos += after_sign;
                self.take_digits();
            }
        }

        let span = Span::new(start as u32, self.pos as u32);
        let text = &self.source[start..self.pos];

        // A number must not run straight into symbol characters: `1abc` is neither
        // a number nor a symbol, and silently lexing it as `1` then `abc` would
        // turn a typo into two tokens the parser then rejects for the wrong reason.
        if self.peek().is_some_and(is_symbol_char) {
            while self.peek().is_some_and(is_symbol_char) {
                self.pos += 1;
            }
            return Err(Error::parse(
                Span::new(start as u32, self.pos as u32),
                "number is followed by symbol characters",
            ));
        }

        if is_float {
            // `parse::<f64>` accepts exactly this grammar's float shape and rounds
            // to nearest, as IEEE 754 requires.
            let Ok(f) = text.parse::<f64>() else {
                return Err(Error::parse(span, "malformed float literal"));
            };
            if !f.is_finite() {
                // A literal that denotes an infinity, e.g. `1e400`. EXPR §2 forbids
                // operations from producing non-finite floats and ABI §6.3.1 rule 5
                // refuses them arriving in a signal; rejecting them here closes the
                // last route in, and does it at deploy time rather than per signal.
                return Err(Error::parse(
                    span,
                    "float literal is not finite; EXPR §2 admits no NaN or infinity",
                ));
            }
            Ok(TokenKind::Literal(Value::Float(f)))
        } else {
            let Ok(n) = text.parse::<i64>() else {
                // EXPR §3.1 requires rejecting integer literals outside i64.
                return Err(Error::parse(span, "integer literal is outside i64"));
            };
            Ok(TokenKind::Literal(Value::Int(n)))
        }
    }

    fn take_digits(&mut self) {
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
    }

    /// `string := '"' char* '"'` with the five escapes of EXPR §3.1.
    fn lex_string(&mut self) -> Result<TokenKind, Error> {
        let open = self.pos;
        self.pos += 1; // the opening quote
        let mut out = String::new();

        loop {
            let Some(b) = self.peek() else {
                return Err(Error::parse(
                    Span::new(open as u32, self.pos as u32),
                    "unterminated string",
                ));
            };
            match b {
                b'"' => {
                    self.pos += 1;
                    return Ok(TokenKind::Literal(Value::Str(out)));
                }
                b'\\' => {
                    let esc_start = self.pos;
                    self.pos += 1;
                    let Some(e) = self.peek() else {
                        return Err(Error::parse(
                            Span::new(open as u32, self.pos as u32),
                            "unterminated string",
                        ));
                    };
                    self.pos += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'u' => out.push(self.lex_unicode_escape(esc_start)?),
                        _ => {
                            return Err(Error::parse(
                                Span::new(esc_start as u32, self.pos as u32),
                                "unknown escape sequence",
                            ));
                        }
                    }
                }
                _ => {
                    // Any other character passes through, multi-byte included.
                    let len = self.char_len();
                    out.push_str(&self.source[self.pos..self.pos + len]);
                    self.pos += len;
                }
            }
        }
    }

    /// `\u{XXXX}` — one to six hex digits, naming any Unicode scalar value.
    ///
    /// The braces in EXPR §3.1's syntax are what make the digit count variable: a
    /// fixed-width form would not need them (JSON and Java spell it `\uXXXX`). Six
    /// digits reach U+10FFFF, the highest scalar value; surrogates and anything
    /// above are rejected, since neither is a scalar value and `char` cannot hold
    /// them.
    fn lex_unicode_escape(&mut self, esc_start: usize) -> Result<char, Error> {
        let bad = |end: usize| {
            Error::parse(
                Span::new(esc_start as u32, end as u32),
                "malformed \\u escape; expected \\u{1-6 hex digits}",
            )
        };

        if self.peek() != Some(b'{') {
            return Err(bad(self.pos));
        }
        self.pos += 1;

        let digits_start = self.pos;
        let mut code = 0u32;
        while self.peek().is_some_and(|b| b.is_ascii_hexdigit()) {
            // At most six digits are consumed before the check below bails, so this
            // cannot overflow: 0xFFFFFF fits u32 with room to spare.
            code = code * 16 + hex_value(self.bytes[self.pos]);
            self.pos += 1;
            if self.pos - digits_start > 6 {
                return Err(bad(self.pos));
            }
        }
        let digits = self.pos - digits_start;
        if digits == 0 {
            // Take a closing brace if one is there, so `\u{}` is reported as the
            // whole malformed escape rather than as the `\u{` prefix.
            if self.peek() == Some(b'}') {
                self.pos += 1;
            }
            return Err(bad(self.pos));
        }
        if self.peek() != Some(b'}') {
            return Err(bad(self.pos));
        }
        self.pos += 1;

        char::from_u32(code).ok_or_else(|| {
            Error::parse(
                Span::new(esc_start as u32, self.pos as u32),
                "\\u escape is not a Unicode scalar value",
            )
        })
    }
}

/// Numeric value of an ASCII hex digit.
fn hex_value(b: u8) -> u32 {
    match b {
        b'0'..=b'9' => u32::from(b - b'0'),
        b'a'..=b'f' => u32::from(b - b'a') + 10,
        _ => u32::from(b - b'A') + 10,
    }
}
