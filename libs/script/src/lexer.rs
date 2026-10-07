//! The tokenizer. The whole source is tokenized up front, which gives the
//! parser unlimited lookahead (for arrow functions). Whether `/` starts a
//! regular expression is decided from the previous token, as most engines'
//! pre-scanners do.

use crate::value::JsStr;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

pub type Name = Rc<str>;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    /// An identifier or keyword. `escaped`: it contained `\u` escapes (so it
    /// can't act as a keyword).
    Ident(Name, bool),
    /// `#name` (class private names).
    Private(Name),
    Num(f64),
    Str(JsStr),
    /// A template piece. `cont`: it starts after a `}` (not a backtick).
    /// `tail`: it ends with a backtick (not `${`). `cooked` is None when it
    /// holds an invalid escape (allowed in tagged templates).
    Template {
        cooked: Option<JsStr>,
        raw: JsStr,
        cont: bool,
        tail: bool,
    },
    Regex {
        pattern: JsStr,
        flags: String,
    },
    Punct(&'static str),
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    /// Byte offsets into the source.
    pub start: u32,
    pub end: u32,
    pub line: u32,
    pub col: u32,
    /// A line terminator came between this token and the previous one.
    pub nl_before: bool,
    /// For strings: the literal contained a legacy octal escape (for strict mode).
    pub legacy_octal: bool,
}

#[derive(Debug, Clone)]
pub struct SyntaxError {
    pub message: String,
    pub line: u32,
    pub col: u32,
}

const PUNCTS: &[&str] = &[
    ">>>=", "...", "===", "!==", "**=", "<<=", ">>=", ">>>", "&&=", "||=", "??=", "=>", "==", "!=", "<=", ">=", "&&",
    "||", "??", "?.", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<", ">>", "**", "{", "}", "(", ")",
    "[", "]", ";", ",", "<", ">", "+", "-", "*", "/", "%", "&", "|", "^", "!", "~", "?", ":", "=", ".", "@",
];

/// Keywords after which an expression (and so a regex) may begin.
const EXPR_KEYWORDS: &[&str] = &[
    "return",
    "typeof",
    "instanceof",
    "in",
    "of",
    "new",
    "delete",
    "void",
    "throw",
    "case",
    "do",
    "else",
    "yield",
    "await",
];

pub fn is_id_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '$' || c == '_' || (!c.is_ascii() && crate::unicode_id::is_id_start(c))
}

pub fn is_id_part(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '$' || c == '_' || (!c.is_ascii() && crate::unicode_id::is_id_continue(c))
}

fn is_line_term(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn is_ws(c: char) -> bool {
    matches!(c, '\t' | '\u{0B}' | '\u{0C}' | ' ' | '\u{A0}' | '\u{FEFF}')
        || (!c.is_ascii() && c.is_whitespace() && !is_line_term(c))
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    line: u32,
    line_start: usize,
    /// For each open `{`: whether it opened a template substitution.
    braces: Vec<bool>,
    tokens: Vec<Token>,
}

pub fn tokenize(src: &str) -> Result<Vec<Token>, SyntaxError> {
    let mut lx = Lexer { src, pos: 0, line: 1, line_start: 0, braces: Vec::new(), tokens: Vec::new() };
    // A hashbang line.
    if src.starts_with("#!") {
        while let Some(c) = lx.peek() {
            if is_line_term(c) {
                break;
            }
            lx.bump();
        }
    }
    loop {
        let nl = lx.skip_trivia()?;
        let start = lx.pos;
        let (line, col) = (lx.line, (lx.pos - lx.line_start) as u32 + 1);
        let mut legacy_octal = false;
        let tok = match lx.peek() {
            None => Tok::Eof,
            Some(c) => lx.token(c, &mut legacy_octal)?,
        };
        let eof = tok == Tok::Eof;
        lx.tokens.push(Token { tok, start: start as u32, end: lx.pos as u32, line, col, nl_before: nl, legacy_octal });
        if eof {
            break;
        }
    }
    Ok(lx.tokens)
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.src[self.pos..].chars().nth(n)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        if is_line_term(c) {
            if c == '\r' && self.peek() == Some('\n') {
                self.pos += 1;
            }
            self.line += 1;
            self.line_start = self.pos;
        }
        Some(c)
    }

    fn err(&self, msg: &str) -> SyntaxError {
        SyntaxError { message: String::from(msg), line: self.line, col: (self.pos - self.line_start) as u32 + 1 }
    }

    /// Skips whitespace and comments; returns whether a line break was seen.
    fn skip_trivia(&mut self) -> Result<bool, SyntaxError> {
        let mut nl = false;
        loop {
            match self.peek() {
                Some(c) if is_ws(c) => {
                    self.bump();
                }
                Some(c) if is_line_term(c) => {
                    nl = true;
                    self.bump();
                }
                Some('/') if self.peek_at(1) == Some('/') => {
                    while let Some(c) = self.peek() {
                        if is_line_term(c) {
                            break;
                        }
                        self.bump();
                    }
                }
                Some('/') if self.peek_at(1) == Some('*') => {
                    self.bump();
                    self.bump();
                    loop {
                        match self.bump() {
                            None => return Err(self.err("unterminated comment")),
                            Some('*') if self.peek() == Some('/') => {
                                self.bump();
                                break;
                            }
                            Some(c) if is_line_term(c) => nl = true,
                            _ => {}
                        }
                    }
                }
                // HTML-like comments (Annex B), at the start of a line.
                Some('<') if self.src[self.pos..].starts_with("<!--") => {
                    while let Some(c) = self.peek() {
                        if is_line_term(c) {
                            break;
                        }
                        self.bump();
                    }
                }
                Some('-') if (nl || self.tokens.is_empty()) && self.src[self.pos..].starts_with("-->") => {
                    while let Some(c) = self.peek() {
                        if is_line_term(c) {
                            break;
                        }
                        self.bump();
                    }
                }
                _ => return Ok(nl),
            }
        }
    }

    fn regex_allowed(&self) -> bool {
        match self.tokens.last().map(|t| &t.tok) {
            None => true,
            Some(Tok::Num(_) | Tok::Str(_) | Tok::Regex { .. } | Tok::Private(_)) => false,
            Some(Tok::Template { tail, .. }) => !*tail,
            Some(Tok::Ident(name, _)) => EXPR_KEYWORDS.contains(&&**name),
            Some(Tok::Punct(p)) => !matches!(*p, ")" | "]" | "}" | "++" | "--"),
            Some(Tok::Eof) => false,
        }
    }

    fn token(&mut self, c: char, legacy_octal: &mut bool) -> Result<Tok, SyntaxError> {
        if is_id_start(c) || c == '\\' {
            let (name, escaped) = self.ident()?;
            return Ok(Tok::Ident(name, escaped));
        }
        if c == '#' {
            self.bump();
            let (name, _) = self.ident()?;
            return Ok(Tok::Private(name));
        }
        if c.is_ascii_digit() || (c == '.' && self.peek_at(1).is_some_and(|d| d.is_ascii_digit())) {
            return self.number();
        }
        if c == '"' || c == '\'' {
            return self.string(c, legacy_octal);
        }
        if c == '`' {
            self.bump();
            return self.template(false);
        }
        if c == '}' && self.braces.last() == Some(&true) {
            self.braces.pop();
            self.bump();
            return self.template(true);
        }
        if c == '/' && self.regex_allowed() {
            return self.regex();
        }
        let rest = &self.src[self.pos..];
        for p in PUNCTS {
            if rest.starts_with(p) {
                // `?.` followed by a digit is `?` then a number.
                if *p == "?." && rest[2..].starts_with(|d: char| d.is_ascii_digit()) {
                    continue;
                }
                self.pos += p.len();
                match *p {
                    "{" => self.braces.push(false),
                    "}" => {
                        self.braces.pop();
                    }
                    _ => {}
                }
                return Ok(Tok::Punct(p));
            }
        }
        Err(self.err(&format!("unexpected character '{c}'")))
    }

    fn ident(&mut self) -> Result<(Name, bool), SyntaxError> {
        let mut s = String::new();
        let mut escaped = false;
        let mut first = true;
        loop {
            match self.peek() {
                Some('\\') => {
                    self.bump();
                    if self.bump() != Some('u') {
                        return Err(self.err("invalid escape in identifier"));
                    }
                    let cp = self.unicode_escape_body()?;
                    let ch = char::from_u32(cp).ok_or_else(|| self.err("invalid identifier escape"))?;
                    if !(if first { is_id_start(ch) } else { is_id_part(ch) }) {
                        return Err(self.err("invalid identifier escape"));
                    }
                    s.push(ch);
                    escaped = true;
                }
                Some(c) if (if first { is_id_start(c) } else { is_id_part(c) }) => {
                    self.bump();
                    s.push(c);
                }
                _ => break,
            }
            first = false;
        }
        if s.is_empty() {
            return Err(self.err("expected an identifier"));
        }
        Ok((Name::from(s), escaped))
    }

    /// After `\u`: `XXXX` or `{X…}`.
    fn unicode_escape_body(&mut self) -> Result<u32, SyntaxError> {
        if self.peek() == Some('{') {
            self.bump();
            let mut v: u32 = 0;
            let mut n = 0;
            loop {
                match self.bump() {
                    Some('}') if n > 0 => break,
                    Some(c) if c.is_ascii_hexdigit() => {
                        v = v.saturating_mul(16).saturating_add(c.to_digit(16).unwrap());
                        n += 1;
                    }
                    _ => return Err(self.err("invalid Unicode escape")),
                }
            }
            if v > 0x10FFFF {
                return Err(self.err("Unicode escape out of range"));
            }
            Ok(v)
        } else {
            let mut v = 0;
            for _ in 0..4 {
                match self.bump() {
                    Some(c) if c.is_ascii_hexdigit() => v = v * 16 + c.to_digit(16).unwrap(),
                    _ => return Err(self.err("invalid Unicode escape")),
                }
            }
            Ok(v)
        }
    }

    fn digits(&mut self, radix: u32, out: &mut String) -> Result<(), SyntaxError> {
        let mut last_sep = true;
        let mut any = false;
        while let Some(c) = self.peek() {
            if c == '_' {
                if last_sep {
                    return Err(self.err("invalid numeric separator"));
                }
                last_sep = true;
                self.bump();
                continue;
            }
            if !c.is_digit(radix) {
                break;
            }
            out.push(c);
            self.bump();
            last_sep = false;
            any = true;
        }
        if any && last_sep {
            return Err(self.err("invalid numeric separator"));
        }
        Ok(())
    }

    fn number(&mut self) -> Result<Tok, SyntaxError> {
        let start = self.pos;
        let c = self.peek().unwrap();
        let next = self.peek_at(1);
        if c == '0' && matches!(next, Some('x' | 'X' | 'o' | 'O' | 'b' | 'B')) {
            let radix = match next.unwrap().to_ascii_lowercase() {
                'x' => 16,
                'o' => 8,
                _ => 2,
            };
            self.bump();
            self.bump();
            let mut s = String::new();
            self.digits(radix, &mut s)?;
            if s.is_empty() {
                return Err(self.err("missing digits"));
            }
            if self.peek() == Some('n') {
                return Err(self.err("BigInt is not supported yet"));
            }
            let mut v = 0.0f64;
            for ch in s.chars() {
                v = v * radix as f64 + ch.to_digit(radix).unwrap() as f64;
            }
            self.check_after_number()?;
            return Ok(Tok::Num(v));
        }
        // Legacy octal (sloppy mode) like 017, or decimal-like 019.
        if c == '0' && next.is_some_and(|d| d.is_ascii_digit()) {
            let mut s = String::new();
            while let Some(d) = self.peek().filter(|d| d.is_ascii_digit()) {
                s.push(d);
                self.bump();
            }
            if s.chars().all(|d| d < '8') {
                let mut v = 0.0;
                for ch in s.chars() {
                    v = v * 8.0 + ch.to_digit(8).unwrap() as f64;
                }
                self.check_after_number()?;
                return Ok(Tok::Num(v));
            }
            // Fall through as decimal: re-scan.
            self.pos = start;
        }
        let mut s = String::new();
        self.digits(10, &mut s)?;
        if self.peek() == Some('n') {
            return Err(self.err("BigInt is not supported yet"));
        }
        if self.peek() == Some('.') {
            self.bump();
            s.push('.');
            self.digits(10, &mut s)?;
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let save = self.pos;
            self.bump();
            let mut e = String::from("e");
            if let Some(sign @ ('+' | '-')) = self.peek() {
                e.push(sign);
                self.bump();
            }
            let before = e.len();
            self.digits(10, &mut e)?;
            if e.len() == before {
                self.pos = save;
                return Err(self.err("missing exponent"));
            }
            s.push_str(&e);
        }
        self.check_after_number()?;
        let v: f64 = if s.starts_with('.') { format!("0{s}").parse() } else { s.parse() }
            .map_err(|_| self.err("invalid number"))?;
        Ok(Tok::Num(v))
    }

    fn check_after_number(&self) -> Result<(), SyntaxError> {
        if self.peek().is_some_and(|c| is_id_start(c) || c.is_ascii_digit()) {
            return Err(self.err("identifier starts immediately after a number"));
        }
        Ok(())
    }

    /// An escape after the backslash, appended to `out`. Returns false for
    /// an escape that is invalid in templates (\1, \x, \u bad).
    fn escape(&mut self, out: &mut Vec<u16>, template: bool, legacy_octal: &mut bool) -> Result<bool, SyntaxError> {
        let Some(c) = self.bump() else { return Err(self.err("unterminated string")) };
        let simple = match c {
            'n' => Some(0x0A),
            't' => Some(0x09),
            'r' => Some(0x0D),
            'b' => Some(0x08),
            'f' => Some(0x0C),
            'v' => Some(0x0B),
            _ => None,
        };
        if let Some(u) = simple {
            out.push(u);
            return Ok(true);
        }
        match c {
            '\r' | '\n' | '\u{2028}' | '\u{2029}' => {} // line continuation
            '0' if !self.peek().is_some_and(|d| d.is_ascii_digit()) => out.push(0),
            '0'..='7' => {
                if template {
                    return Ok(false);
                }
                *legacy_octal = true;
                let mut v = c.to_digit(8).unwrap();
                let max_len = if c <= '3' { 3 } else { 2 };
                let mut n = 1;
                while n < max_len {
                    match self.peek().and_then(|d| d.to_digit(8)) {
                        Some(d) => {
                            v = v * 8 + d;
                            self.bump();
                            n += 1;
                        }
                        None => break,
                    }
                }
                out.push(v as u16);
            }
            '8' | '9' => {
                if template {
                    return Ok(false);
                }
                *legacy_octal = true;
                out.push(c as u16);
            }
            'x' => {
                let mut v = 0;
                for _ in 0..2 {
                    match self.peek() {
                        Some(h) if h.is_ascii_hexdigit() => {
                            v = v * 16 + h.to_digit(16).unwrap();
                            self.bump();
                        }
                        _ => {
                            if template {
                                return Ok(false);
                            }
                            return Err(self.err("invalid hexadecimal escape"));
                        }
                    }
                }
                out.push(v as u16);
            }
            'u' => {
                let save = self.pos;
                match self.unicode_escape_body() {
                    Ok(cp) => push_code_point(out, cp),
                    Err(e) => {
                        if template {
                            self.pos = save;
                            return Ok(false);
                        }
                        return Err(e);
                    }
                }
            }
            c => {
                let mut buf = [0u16; 2];
                out.extend_from_slice(c.encode_utf16(&mut buf));
            }
        }
        Ok(true)
    }

    fn string(&mut self, quote: char, legacy_octal: &mut bool) -> Result<Tok, SyntaxError> {
        self.bump();
        let mut out = Vec::new();
        loop {
            match self.peek() {
                None => return Err(self.err("unterminated string")),
                Some(c) if c == quote => {
                    self.bump();
                    break;
                }
                Some('\\') => {
                    self.bump();
                    self.escape(&mut out, false, legacy_octal)?;
                }
                Some('\n' | '\r') => return Err(self.err("unterminated string")),
                Some(c) => {
                    self.bump();
                    let mut buf = [0u16; 2];
                    out.extend_from_slice(c.encode_utf16(&mut buf));
                }
            }
        }
        Ok(Tok::Str(JsStr::from_units(out)))
    }

    fn template(&mut self, cont: bool) -> Result<Tok, SyntaxError> {
        let mut cooked = Vec::new();
        let mut valid = true;
        let raw_start = self.pos;
        let mut dummy = false;
        let (tail, raw_end) = loop {
            match self.peek() {
                None => return Err(self.err("unterminated template literal")),
                Some('`') => {
                    let end = self.pos;
                    self.bump();
                    break (true, end);
                }
                Some('$') if self.peek_at(1) == Some('{') => {
                    let end = self.pos;
                    self.bump();
                    self.bump();
                    self.braces.push(true);
                    break (false, end);
                }
                Some('\\') => {
                    self.bump();
                    if !self.escape(&mut cooked, true, &mut dummy)? {
                        valid = false;
                    }
                }
                Some('\r') => {
                    // CR and CRLF are normalised to LF.
                    self.bump();
                    cooked.push(0x0A);
                }
                Some(c) => {
                    self.bump();
                    let mut buf = [0u16; 2];
                    cooked.extend_from_slice(c.encode_utf16(&mut buf));
                }
            }
        };
        let raw: Vec<u16> =
            self.src[raw_start..raw_end].replace("\r\n", "\n").replace('\r', "\n").encode_utf16().collect();
        Ok(Tok::Template { cooked: valid.then(|| JsStr::from_units(cooked)), raw: JsStr::from_units(raw), cont, tail })
    }

    fn regex(&mut self) -> Result<Tok, SyntaxError> {
        self.bump();
        let start = self.pos;
        let mut in_class = false;
        loop {
            match self.bump() {
                None => return Err(self.err("unterminated regular expression")),
                Some(c) if is_line_term(c) => return Err(self.err("unterminated regular expression")),
                Some('\\') => {
                    if self.bump().is_none_or(is_line_term) {
                        return Err(self.err("unterminated regular expression"));
                    }
                }
                Some('[') => in_class = true,
                Some(']') => in_class = false,
                Some('/') if !in_class => break,
                _ => {}
            }
        }
        let pattern = JsStr::from(&self.src[start..self.pos - 1]);
        let mut flags = String::new();
        while let Some(c) = self.peek().filter(|c| is_id_part(*c)) {
            flags.push(c);
            self.bump();
        }
        Ok(Tok::Regex { pattern, flags })
    }
}

pub fn push_code_point(out: &mut Vec<u16>, cp: u32) {
    if cp >= 0x10000 {
        let c = cp - 0x10000;
        out.push(0xD800 | (c >> 10) as u16);
        out.push(0xDC00 | (c & 0x3FF) as u16);
    } else {
        out.push(cp as u16);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<Tok> {
        tokenize(src).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn basics() {
        let t = kinds("let x = a / 2 / b; y = /ab+c/gi.test(s)");
        assert!(matches!(&t[4], Tok::Punct("/")) && matches!(&t[6], Tok::Punct("/")));
        assert!(t.iter().any(|t| matches!(t, Tok::Regex { flags, .. } if flags == "gi")));
        let t = kinds("`a${b + `c${d}`}e`");
        assert_eq!(t.len(), 8, "{t:?}");
        let t = kinds("0x1F 1_000 .5 1e3 017 'a\\u{1F600}\\x41'");
        assert_eq!(t[0], Tok::Num(31.0));
        assert_eq!(t[1], Tok::Num(1000.0));
        assert_eq!(t[2], Tok::Num(0.5));
        assert_eq!(t[3], Tok::Num(1000.0));
        assert_eq!(t[4], Tok::Num(15.0));
        if let Tok::Str(s) = &t[5] {
            assert_eq!(s.units(), &[0x61, 0xD83D, 0xDE00, 0x41]);
        } else {
            panic!()
        }
        let toks = tokenize("a\nb").unwrap();
        assert!(toks[1].nl_before);
        assert!(tokenize("'abc").is_err());
        assert!(kinds("a?.5:1").contains(&Tok::Punct("?")));
    }
}
