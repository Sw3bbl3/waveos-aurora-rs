//! Regular expressions: JavaScript pattern syntax (with the web-compatibility
//! leniencies of Annex B) compiled to a program for a backtracking matcher.
//!
//! Supports alternation, greedy and lazy quantifiers, classes, \d\w\s\b\B,
//! Unicode escapes and (with the `u` flag) code points and \p{…}, capturing,
//! non-capturing and named groups, backreferences, lookahead and lookbehind,
//! and the i, m, s, u, g, y and d flags.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, Default)]
pub struct Flags {
    pub global: bool,
    pub ignore_case: bool,
    pub multiline: bool,
    pub dot_all: bool,
    pub unicode: bool,
    pub sticky: bool,
    pub has_indices: bool,
}

impl Flags {
    pub fn parse(s: &str) -> Result<Flags, String> {
        let mut f = Flags::default();
        for c in s.chars() {
            let slot = match c {
                'g' => &mut f.global,
                'i' => &mut f.ignore_case,
                'm' => &mut f.multiline,
                's' => &mut f.dot_all,
                'u' | 'v' => &mut f.unicode,
                'y' => &mut f.sticky,
                'd' => &mut f.has_indices,
                _ => return Err(format!("Invalid regular expression flags '{s}'")),
            };
            if *slot {
                return Err(format!("Invalid regular expression flags '{s}'"));
            }
            *slot = true;
        }
        Ok(f)
    }
}

#[derive(Clone, Debug)]
enum ClassItem {
    Range(u32, u32),
    Digit(bool),
    Word(bool),
    Space(bool),
    Prop(Prop, bool),
}

#[derive(Clone, Copy, Debug)]
enum Prop {
    Letter,
    Upper,
    Lower,
    Number,
    Digit,
    Alphabetic,
    WhiteSpace,
    Punctuation,
    Any,
    Ascii,
    Emoji,
}

#[derive(Clone, Debug)]
struct Class {
    items: Vec<ClassItem>,
    negate: bool,
}

#[derive(Clone, Debug)]
enum Node {
    Char(u32),
    Any,
    Class(Class),
    Start,
    End,
    WordBoundary(bool),
    Group(Box<Node>, Option<usize>),
    Backref(usize),
    NamedBackref(String),
    Look { node: Box<Node>, ahead: bool, negate: bool },
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat { node: Box<Node>, min: u32, max: Option<u32>, greedy: bool },
}

#[derive(Clone, Copy, Debug)]
enum Inst {
    Char(u32),
    Any,
    Class(u32),
    /// Try the first branch, backtrack into the second.
    Split(u32, u32),
    Jmp(u32),
    Save(u32),
    Start,
    End,
    WordBoundary(bool),
    Backref(u32),
    /// A lookaround: its body runs from pc+1 to `end` (a Match).
    Look {
        ahead: bool,
        negate: bool,
        end: u32,
    },
    /// Records the position at a loop iteration's start…
    Mark(u32),
    /// …and fails if the iteration consumed nothing.
    Progress(u32),
    Match,
}

pub struct Program {
    insts: Vec<Inst>,
    classes: Vec<Class>,
    /// Including group 0 (the whole match).
    pub n_groups: usize,
    pub names: Vec<(String, usize)>,
    pub flags: Flags,
    n_marks: u32,
}

impl core::fmt::Debug for Program {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "Program({} insts, {} groups)", self.insts.len(), self.n_groups)
    }
}

/// Capture positions (code unit offsets), by group.
pub type Captures = Vec<Option<(usize, usize)>>;

pub fn compile(pattern: &[u16], flags: &str) -> Result<Program, String> {
    let flags = Flags::parse(flags)?;
    let cps: Vec<u32> = if flags.unicode {
        char::decode_utf16(pattern.iter().copied()).map(|r| r.map(|c| c as u32).unwrap_or(0xFFFD)).collect()
    } else {
        pattern.iter().map(|&u| u as u32).collect()
    };
    // Count groups and collect names first (for forward references).
    let mut names = Vec::new();
    let n_groups = count_groups(&cps, &mut names) + 1;
    let mut p = PatParser { s: &cps, pos: 0, flags, group: 0, n_groups, names: &names, depth: 0 };
    let node = p.disjunction()?;
    if p.pos < cps.len() {
        return Err(String::from(if cps[p.pos] == ')' as u32 { "Unmatched ')'" } else { "Unexpected character" }));
    }
    let mut c = Emitter { insts: Vec::new(), classes: Vec::new(), marks: 0, names: &names, backward: false };
    c.insts.push(Inst::Save(0));
    c.emit(&node)?;
    c.insts.push(Inst::Save(1));
    c.insts.push(Inst::Match);
    let (insts, classes, n_marks) = (c.insts, c.classes, c.marks);
    Ok(Program { insts, classes, n_groups, names, flags, n_marks })
}

fn count_groups(s: &[u32], names: &mut Vec<(String, usize)>) -> usize {
    let mut n = 0;
    let mut i = 0;
    let mut in_class = false;
    while i < s.len() {
        let c = s[i];
        if c == '\\' as u32 {
            i += 2;
            continue;
        }
        if in_class {
            if c == ']' as u32 {
                in_class = false;
            }
        } else if c == '[' as u32 {
            in_class = true;
        } else if c == '(' as u32 {
            if i + 1 < s.len() && s[i + 1] == '?' as u32 {
                if i + 2 < s.len()
                    && s[i + 2] == '<' as u32
                    && i + 3 < s.len()
                    && s[i + 3] != '=' as u32
                    && s[i + 3] != '!' as u32
                {
                    n += 1;
                    let mut j = i + 3;
                    let mut name = String::new();
                    while j < s.len() && s[j] != '>' as u32 {
                        name.push(char::from_u32(s[j]).unwrap_or('?'));
                        j += 1;
                    }
                    names.push((name, n));
                }
            } else {
                n += 1;
            }
        }
        i += 1;
    }
    n
}

struct PatParser<'a> {
    s: &'a [u32],
    pos: usize,
    flags: Flags,
    group: usize,
    n_groups: usize,
    names: &'a [(String, usize)],
    depth: u32,
}

fn is_syntax(c: u32) -> bool {
    matches!(
        char::from_u32(c),
        Some('^' | '$' | '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '/')
    )
}

impl<'a> PatParser<'a> {
    fn peek(&self) -> Option<u32> {
        self.s.get(self.pos).copied()
    }

    fn peek_is(&self, c: char) -> bool {
        self.peek() == Some(c as u32)
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek_is(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn disjunction(&mut self) -> Result<Node, String> {
        self.depth += 1;
        if self.depth > 200 {
            return Err(String::from("Regular expression too deeply nested"));
        }
        let mut alts = vec![self.alternative()?];
        while self.eat('|') {
            alts.push(self.alternative()?);
        }
        self.depth -= 1;
        Ok(if alts.len() == 1 { alts.pop().unwrap() } else { Node::Alt(alts) })
    }

    fn alternative(&mut self) -> Result<Node, String> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' as u32 || c == ')' as u32 {
                break;
            }
            let atom = self.term()?;
            items.push(atom);
        }
        Ok(if items.len() == 1 { items.pop().unwrap() } else { Node::Concat(items) })
    }

    fn term(&mut self) -> Result<Node, String> {
        let c = self.peek().unwrap();
        let ch = char::from_u32(c).unwrap_or('\u{FFFD}');
        // Assertions.
        match ch {
            '^' => {
                self.pos += 1;
                return Ok(Node::Start);
            }
            '$' => {
                self.pos += 1;
                return Ok(Node::End);
            }
            '\\' if matches!(self.s.get(self.pos + 1).and_then(|&c| char::from_u32(c)), Some('b' | 'B')) => {
                let b = self.s[self.pos + 1] == 'b' as u32;
                self.pos += 2;
                return Ok(Node::WordBoundary(b));
            }
            '(' if self.s.get(self.pos + 1) == Some(&('?' as u32)) => {
                let look = match (self.s.get(self.pos + 2).copied(), self.s.get(self.pos + 3).copied()) {
                    (Some(0x3D), _) => Some((true, false, 3)),
                    (Some(0x21), _) => Some((true, true, 3)),
                    (Some(0x3C), Some(0x3D)) => Some((false, false, 4)),
                    (Some(0x3C), Some(0x21)) => Some((false, true, 4)),
                    _ => None,
                };
                if let Some((ahead, negate, skip)) = look {
                    self.pos += skip;
                    let inner = self.disjunction()?;
                    if !self.eat(')') {
                        return Err(String::from("Unterminated group"));
                    }
                    let node = Node::Look { node: Box::new(inner), ahead, negate };
                    // Annex B: lookaheads may be quantified (without u).
                    if ahead && !self.flags.unicode {
                        return self.quantifier(node);
                    }
                    return Ok(node);
                }
            }
            _ => {}
        }
        let atom = self.atom()?;
        self.quantifier(atom)
    }

    fn quantifier(&mut self, atom: Node) -> Result<Node, String> {
        let (min, max) = match self.peek().and_then(char::from_u32) {
            Some('*') => {
                self.pos += 1;
                (0, None)
            }
            Some('+') => {
                self.pos += 1;
                (1, None)
            }
            Some('?') => {
                self.pos += 1;
                (0, Some(1))
            }
            Some('{') => match self.braces()? {
                Some(q) => q,
                None => return Ok(atom),
            },
            _ => return Ok(atom),
        };
        if let Some(m) = max {
            if m < min {
                return Err(String::from("numbers out of order in {} quantifier"));
            }
        }
        if matches!(atom, Node::Start | Node::End | Node::WordBoundary(_))
            || matches!(atom, Node::Look { ahead: false, .. })
        {
            return Err(String::from("Nothing to repeat"));
        }
        let greedy = !self.eat('?');
        Ok(Node::Repeat { node: Box::new(atom), min, max, greedy })
    }

    /// `{n}`, `{n,}`, `{n,m}`; None if this brace isn't a quantifier.
    fn braces(&mut self) -> Result<Option<(u32, Option<u32>)>, String> {
        let save = self.pos;
        self.pos += 1;
        let num = |p: &mut Self| -> Option<u32> {
            let start = p.pos;
            let mut v: u64 = 0;
            while let Some(d) = p.peek().filter(|c| (0x30..=0x39).contains(c)) {
                v = (v * 10 + (d - 0x30) as u64).min(u32::MAX as u64);
                p.pos += 1;
            }
            (p.pos > start).then_some(v as u32)
        };
        let Some(min) = num(self) else {
            self.pos = save;
            if self.flags.unicode {
                return Err(String::from("Incomplete quantifier"));
            }
            return Ok(None);
        };
        let max = if self.eat(',') { num(self) } else { Some(min) };
        if !self.eat('}') {
            self.pos = save;
            if self.flags.unicode {
                return Err(String::from("Incomplete quantifier"));
            }
            return Ok(None);
        }
        Ok(Some((min, max)))
    }

    fn atom(&mut self) -> Result<Node, String> {
        let c = self.peek().unwrap();
        let ch = char::from_u32(c).unwrap_or('\u{FFFD}');
        match ch {
            '.' => {
                self.pos += 1;
                Ok(Node::Any)
            }
            '(' => {
                self.pos += 1;
                let mut capture = true;
                if self.eat('?') {
                    if self.eat(':') {
                        capture = false;
                    } else if self.eat('<') {
                        while !self.eat('>') {
                            if self.peek().is_none() {
                                return Err(String::from("Invalid capture group name"));
                            }
                            self.pos += 1;
                        }
                    } else {
                        return Err(String::from("Invalid group"));
                    }
                }
                let index = if capture {
                    self.group += 1;
                    Some(self.group)
                } else {
                    None
                };
                let inner = self.disjunction()?;
                if !self.eat(')') {
                    return Err(String::from("Unterminated group"));
                }
                Ok(Node::Group(Box::new(inner), index))
            }
            ')' => Err(String::from("Unmatched ')'")),
            '[' => {
                self.pos += 1;
                Ok(Node::Class(self.class()?))
            }
            '*' | '+' | '?' => Err(String::from("Nothing to repeat")),
            '{' => {
                if self.flags.unicode {
                    return Err(String::from("Lone quantifier brackets"));
                }
                // Annex B: a literal brace unless it forms a quantifier.
                let save = self.pos;
                if self.braces()?.is_some() {
                    self.pos = save;
                    return Err(String::from("Nothing to repeat"));
                }
                self.pos = save + 1;
                Ok(Node::Char(c))
            }
            '}' | ']' if self.flags.unicode => Err(String::from("Lone quantifier brackets")),
            '\\' => {
                self.pos += 1;
                self.atom_escape()
            }
            _ => {
                self.pos += 1;
                Ok(Node::Char(c))
            }
        }
    }

    fn atom_escape(&mut self) -> Result<Node, String> {
        let Some(c) = self.peek() else { return Err(String::from("\\ at end of pattern")) };
        let ch = char::from_u32(c).unwrap_or('\u{FFFD}');
        if ch.is_ascii_digit() && ch != '0' {
            // A backreference, or (Annex B) an octal escape.
            let start = self.pos;
            let mut n: usize = 0;
            while let Some(d) = self.peek().filter(|c| (0x30..=0x39).contains(c)) {
                n = n.saturating_mul(10).saturating_add((d - 0x30) as usize);
                self.pos += 1;
            }
            if n < self.n_groups {
                return Ok(Node::Backref(n));
            }
            if self.flags.unicode {
                return Err(String::from("Invalid escape"));
            }
            self.pos = start;
            return Ok(Node::Char(self.legacy_octal()));
        }
        if ch == 'k' {
            if self.s.get(self.pos + 1) == Some(&('<' as u32)) && (!self.names.is_empty() || self.flags.unicode) {
                self.pos += 2;
                let mut name = String::new();
                while !self.eat('>') {
                    let Some(c) = self.peek() else { return Err(String::from("Invalid named reference")) };
                    name.push(char::from_u32(c).unwrap_or('?'));
                    self.pos += 1;
                }
                if !self.names.iter().any(|(n, _)| *n == name) {
                    return Err(String::from("Invalid named capture referenced"));
                }
                return Ok(Node::NamedBackref(name));
            }
            if self.flags.unicode || !self.names.is_empty() {
                return Err(String::from("Invalid named reference"));
            }
            self.pos += 1;
            return Ok(Node::Char(c));
        }
        if let Some(item) = self.class_escape()? {
            return Ok(Node::Class(Class { items: vec![item], negate: false }));
        }
        Ok(Node::Char(self.char_escape()?))
    }

    fn legacy_octal(&mut self) -> u32 {
        let mut v = 0;
        let mut n = 0;
        while n < 3 {
            match self.peek().filter(|c| (0x30..=0x37).contains(c)) {
                Some(d) if v * 8 + (d - 0x30) <= 0o377 => {
                    v = v * 8 + (d - 0x30);
                    self.pos += 1;
                    n += 1;
                }
                _ => break,
            }
        }
        if n == 0 {
            // \8 or \9: the digit itself.
            let c = self.peek().unwrap();
            self.pos += 1;
            return c;
        }
        v
    }

    /// \d \D \w \W \s \S \p{…} \P{…}
    fn class_escape(&mut self) -> Result<Option<ClassItem>, String> {
        let Some(c) = self.peek() else { return Ok(None) };
        let item = match char::from_u32(c) {
            Some('d') => ClassItem::Digit(false),
            Some('D') => ClassItem::Digit(true),
            Some('w') => ClassItem::Word(false),
            Some('W') => ClassItem::Word(true),
            Some('s') => ClassItem::Space(false),
            Some('S') => ClassItem::Space(true),
            Some(p @ ('p' | 'P')) if self.flags.unicode => {
                self.pos += 1;
                if !self.eat('{') {
                    return Err(String::from("Invalid property name"));
                }
                let mut name = String::new();
                while !self.eat('}') {
                    let Some(c) = self.peek() else { return Err(String::from("Invalid property name")) };
                    name.push(char::from_u32(c).unwrap_or('?'));
                    self.pos += 1;
                }
                let value = name.rsplit('=').next().unwrap_or("");
                let prop = match value {
                    "L" | "Letter" => Prop::Letter,
                    "Lu" | "Uppercase_Letter" | "Uppercase" => Prop::Upper,
                    "Ll" | "Lowercase_Letter" | "Lowercase" => Prop::Lower,
                    "N" | "Number" => Prop::Number,
                    "Nd" | "Decimal_Number" | "digit" => Prop::Digit,
                    "Alphabetic" | "Alpha" => Prop::Alphabetic,
                    "White_Space" | "space" => Prop::WhiteSpace,
                    "P" | "Punctuation" | "punct" => Prop::Punctuation,
                    "Any" => Prop::Any,
                    "ASCII" => Prop::Ascii,
                    "Emoji" | "Emoji_Presentation" | "Extended_Pictographic" => Prop::Emoji,
                    "Latin" | "Script=Latin" => Prop::Letter,
                    _ => return Err(format!("Invalid property name {name}")),
                };
                return Ok(Some(ClassItem::Prop(prop, p == 'P')));
            }
            _ => return Ok(None),
        };
        self.pos += 1;
        Ok(Some(item))
    }

    /// A character escape after `\` (the escape letter is current).
    fn char_escape(&mut self) -> Result<u32, String> {
        let c = self.peek().unwrap();
        self.pos += 1;
        let ch = char::from_u32(c).unwrap_or('\u{FFFD}');
        Ok(match ch {
            'n' => 0x0A,
            'r' => 0x0D,
            't' => 0x09,
            'v' => 0x0B,
            'f' => 0x0C,
            '0' if !self.peek().is_some_and(|d| (0x30..=0x39).contains(&d)) => 0,
            '0' => {
                if self.flags.unicode {
                    return Err(String::from("Invalid decimal escape"));
                }
                self.pos -= 1;
                self.legacy_octal()
            }
            'c' => match self.peek().and_then(char::from_u32) {
                Some(l) if l.is_ascii_alphabetic() => {
                    self.pos += 1;
                    (l as u32) % 32
                }
                _ => {
                    if self.flags.unicode {
                        return Err(String::from("Invalid unicode escape"));
                    }
                    self.pos -= 1;
                    '\\' as u32
                }
            },
            'x' => {
                let h = self.hex(2);
                match h {
                    Some(v) => v,
                    None => {
                        if self.flags.unicode {
                            return Err(String::from("Invalid escape"));
                        }
                        'x' as u32
                    }
                }
            }
            'u' => {
                if self.flags.unicode && self.eat('{') {
                    let mut v: u32 = 0;
                    let mut n = 0;
                    while !self.eat('}') {
                        let d = self.peek().and_then(char::from_u32).and_then(|c| c.to_digit(16));
                        let Some(d) = d else { return Err(String::from("Invalid Unicode escape")) };
                        v = v.saturating_mul(16).saturating_add(d);
                        n += 1;
                        self.pos += 1;
                    }
                    if n == 0 || v > 0x10FFFF {
                        return Err(String::from("Invalid Unicode escape"));
                    }
                    v
                } else {
                    match self.hex(4) {
                        Some(hi) => {
                            // A surrogate pair written as two escapes (u mode).
                            if self.flags.unicode && (0xD800..0xDC00).contains(&hi) && self.peek_is('\\') {
                                let save = self.pos;
                                self.pos += 1;
                                if self.eat('u') {
                                    if let Some(lo) = self.hex(4) {
                                        if (0xDC00..0xE000).contains(&lo) {
                                            return Ok(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00));
                                        }
                                    }
                                }
                                self.pos = save;
                            }
                            hi
                        }
                        None => {
                            if self.flags.unicode {
                                return Err(String::from("Invalid Unicode escape"));
                            }
                            'u' as u32
                        }
                    }
                }
            }
            _ => {
                if self.flags.unicode && !is_syntax(c) && ch != '-' {
                    return Err(String::from("Invalid escape"));
                }
                c
            }
        })
    }

    fn hex(&mut self, n: usize) -> Option<u32> {
        let mut v = 0;
        for i in 0..n {
            let d = self.s.get(self.pos + i).and_then(|&c| char::from_u32(c)).and_then(|c| c.to_digit(16))?;
            v = v * 16 + d;
        }
        self.pos += n;
        Some(v)
    }

    fn class(&mut self) -> Result<Class, String> {
        let negate = self.eat('^');
        let mut items = Vec::new();
        loop {
            let Some(c) = self.peek() else { return Err(String::from("Unterminated character class")) };
            if c == ']' as u32 {
                self.pos += 1;
                break;
            }
            let a = self.class_atom()?;
            if self.peek_is('-') && self.s.get(self.pos + 1).is_some_and(|&c| c != ']' as u32) {
                self.pos += 1;
                let b = self.class_atom()?;
                match (a, b) {
                    (Ok(lo), Ok(hi)) => {
                        if lo > hi {
                            return Err(String::from("Range out of order in character class"));
                        }
                        items.push(ClassItem::Range(lo, hi));
                    }
                    (a, b) => {
                        if self.flags.unicode {
                            return Err(String::from("Invalid character class"));
                        }
                        for x in [a, b] {
                            match x {
                                Ok(c) => items.push(ClassItem::Range(c, c)),
                                Err(item) => items.push(item),
                            }
                        }
                        items.push(ClassItem::Range('-' as u32, '-' as u32));
                    }
                }
                continue;
            }
            match a {
                Ok(c) => items.push(ClassItem::Range(c, c)),
                Err(item) => items.push(item),
            }
        }
        Ok(Class { items, negate })
    }

    /// A class member: a character (Ok) or an escape class (Err).
    fn class_atom(&mut self) -> Result<Result<u32, ClassItem>, String> {
        let c = self.peek().unwrap();
        self.pos += 1;
        if c != '\\' as u32 {
            return Ok(Ok(c));
        }
        if let Some(item) = self.class_escape()? {
            return Ok(Err(item));
        }
        let Some(e) = self.peek() else { return Err(String::from("\\ at end of pattern")) };
        match char::from_u32(e) {
            Some('b') => {
                self.pos += 1;
                Ok(Ok(8))
            }
            Some('-') if self.flags.unicode => {
                self.pos += 1;
                Ok(Ok('-' as u32))
            }
            Some(d) if d.is_ascii_digit() && !self.flags.unicode => Ok(Ok(self.legacy_octal())),
            Some('c') if !self.flags.unicode => {
                // Annex B: \c with a digit or _ inside a class.
                if let Some(l) = self.s.get(self.pos + 1).and_then(|&c| char::from_u32(c)) {
                    if l.is_ascii_digit() || l == '_' {
                        self.pos += 2;
                        return Ok(Ok((l as u32) % 32));
                    }
                }
                Ok(Ok(self.char_escape()?))
            }
            _ => Ok(Ok(self.char_escape()?)),
        }
    }
}

struct Emitter<'a> {
    insts: Vec<Inst>,
    classes: Vec<Class>,
    marks: u32,
    names: &'a [(String, usize)],
    /// Inside a lookbehind: sequences are emitted right to left.
    backward: bool,
}

const MAX_INSTS: usize = 200_000;

impl<'a> Emitter<'a> {
    fn pc(&self) -> u32 {
        self.insts.len() as u32
    }

    fn emit(&mut self, n: &Node) -> Result<(), String> {
        if self.insts.len() > MAX_INSTS {
            return Err(String::from("Regular expression too large"));
        }
        match n {
            Node::Char(c) => self.insts.push(Inst::Char(*c)),
            Node::Any => self.insts.push(Inst::Any),
            Node::Class(c) => {
                self.classes.push(c.clone());
                self.insts.push(Inst::Class(self.classes.len() as u32 - 1));
            }
            Node::Start => self.insts.push(Inst::Start),
            Node::End => self.insts.push(Inst::End),
            Node::WordBoundary(b) => self.insts.push(Inst::WordBoundary(*b)),
            Node::Group(inner, idx) => {
                // Backwards, the end is reached first.
                let (first, second) = if self.backward { (1, 0) } else { (0, 1) };
                if let Some(i) = idx {
                    self.insts.push(Inst::Save(*i as u32 * 2 + first));
                }
                self.emit(inner)?;
                if let Some(i) = idx {
                    self.insts.push(Inst::Save(*i as u32 * 2 + second));
                }
            }
            Node::Backref(i) => self.insts.push(Inst::Backref(*i as u32)),
            Node::NamedBackref(name) => {
                let i = self.names.iter().find(|(n, _)| n == name).map(|(_, i)| *i).unwrap_or(0);
                self.insts.push(Inst::Backref(i as u32));
            }
            Node::Look { node, ahead, negate } => {
                let at = self.insts.len();
                self.insts.push(Inst::Look { ahead: *ahead, negate: *negate, end: 0 });
                let saved = self.backward;
                self.backward = !*ahead;
                let r = self.emit(node);
                self.backward = saved;
                r?;
                self.insts.push(Inst::Match);
                let end = self.pc();
                self.insts[at] = Inst::Look { ahead: *ahead, negate: *negate, end };
            }
            Node::Concat(items) => {
                if self.backward {
                    for i in items.iter().rev() {
                        self.emit(i)?;
                    }
                } else {
                    for i in items {
                        self.emit(i)?;
                    }
                }
            }
            Node::Alt(alts) => {
                let mut jumps = Vec::new();
                for (i, a) in alts.iter().enumerate() {
                    if i + 1 < alts.len() {
                        let split = self.insts.len();
                        self.insts.push(Inst::Split(0, 0));
                        self.emit(a)?;
                        jumps.push(self.insts.len());
                        self.insts.push(Inst::Jmp(0));
                        let next = self.pc();
                        self.insts[split] = Inst::Split(split as u32 + 1, next);
                    } else {
                        self.emit(a)?;
                    }
                }
                let end = self.pc();
                for j in jumps {
                    self.insts[j] = Inst::Jmp(end);
                }
            }
            Node::Repeat { node, min, max, greedy } => {
                // Captures inside a repeated group reset each iteration.
                let groups = groups_in(node);
                for _ in 0..*min {
                    self.reset_groups(&groups);
                    self.emit(node)?;
                    if self.insts.len() > MAX_INSTS {
                        return Err(String::from("Regular expression too large"));
                    }
                }
                match max {
                    None => {
                        // L: split body, end; body: mark; node; progress; jmp L
                        let mark = self.marks;
                        self.marks += 1;
                        let top = self.insts.len();
                        self.insts.push(Inst::Split(0, 0));
                        let body = self.pc();
                        self.insts.push(Inst::Mark(mark));
                        self.reset_groups(&groups);
                        self.emit(node)?;
                        self.insts.push(Inst::Progress(mark));
                        self.insts.push(Inst::Jmp(top as u32));
                        let end = self.pc();
                        self.insts[top] = if *greedy { Inst::Split(body, end) } else { Inst::Split(end, body) };
                    }
                    Some(max) => {
                        let extra = max - min;
                        let mut splits = Vec::new();
                        for _ in 0..extra {
                            let s = self.insts.len();
                            self.insts.push(Inst::Split(0, 0));
                            splits.push(s);
                            let mark = self.marks;
                            self.marks += 1;
                            self.insts.push(Inst::Mark(mark));
                            self.reset_groups(&groups);
                            self.emit(node)?;
                            self.insts.push(Inst::Progress(mark));
                            if self.insts.len() > MAX_INSTS {
                                return Err(String::from("Regular expression too large"));
                            }
                        }
                        let end = self.pc();
                        for s in splits {
                            let body = s as u32 + 1;
                            self.insts[s] = if *greedy { Inst::Split(body, end) } else { Inst::Split(end, body) };
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn reset_groups(&mut self, groups: &[usize]) {
        for &g in groups {
            // Saving an impossible position clears the group (see `exec`).
            self.insts.push(Inst::Save(u32::MAX - g as u32));
        }
    }
}

fn groups_in(n: &Node) -> Vec<usize> {
    let mut out = Vec::new();
    fn walk(n: &Node, out: &mut Vec<usize>) {
        match n {
            Node::Group(inner, idx) => {
                if let Some(i) = idx {
                    out.push(*i);
                }
                walk(inner, out);
            }
            Node::Look { node, .. } => walk(node, out),
            Node::Concat(v) | Node::Alt(v) => v.iter().for_each(|x| walk(x, out)),
            Node::Repeat { node, .. } => walk(node, out),
            _ => {}
        }
    }
    walk(n, &mut out);
    out
}

// ---------------------------------------------------------------- matching

fn is_line_term(c: u32) -> bool {
    matches!(c, 0x0A | 0x0D | 0x2028 | 0x2029)
}

fn is_word(c: u32) -> bool {
    c < 128 && (char::from_u32(c).unwrap().is_ascii_alphanumeric() || c == '_' as u32)
}

fn is_space(c: u32) -> bool {
    matches!(
        c,
        0x09 | 0x0A | 0x0B | 0x0C | 0x0D | 0x20 | 0xA0 | 0x1680 | 0x2000
            ..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF
    )
}

fn simple_upper(c: u32) -> u32 {
    let Some(ch) = char::from_u32(c) else { return c };
    let mut it = ch.to_uppercase();
    match (it.next(), it.next()) {
        (Some(u), None) => {
            let u = u as u32;
            // Non-ASCII never maps to ASCII (Canonicalize).
            if c >= 128 && u < 128 {
                c
            } else {
                u
            }
        }
        _ => c,
    }
}

fn simple_fold(c: u32) -> u32 {
    let Some(ch) = char::from_u32(c) else { return c };
    let mut it = ch.to_lowercase();
    match (it.next(), it.next()) {
        (Some(l), None) => l as u32,
        _ => c,
    }
}

struct Matcher<'a> {
    prog: &'a Program,
    input: &'a [u16],
    steps: u64,
}

const MAX_STEPS: u64 = 50_000_000;

#[derive(Clone)]
struct Thread {
    pc: u32,
    pos: usize,
    caps: Vec<usize>,
    marks: Vec<usize>,
}

const NONE: usize = usize::MAX;

impl Program {
    /// Matches at exactly `start`.
    pub fn match_at(&self, input: &[u16], start: usize) -> Option<Captures> {
        let mut m = Matcher { prog: self, input, steps: 0 };
        let mut caps = vec![NONE; self.n_groups * 2];
        let mut marks = vec![NONE; self.n_marks as usize];
        if m.run(0, start, &mut caps, &mut marks, false).is_some() {
            Some(
                (0..self.n_groups)
                    .map(|g| (caps[g * 2] != NONE && caps[g * 2 + 1] != NONE).then(|| (caps[g * 2], caps[g * 2 + 1])))
                    .collect(),
            )
        } else {
            None
        }
    }

    /// Searches from `start` onwards (or only at `start` when sticky).
    pub fn exec(&self, input: &[u16], start: usize) -> Option<Captures> {
        if self.flags.sticky {
            return self.match_at(input, start);
        }
        let mut i = start;
        while i <= input.len() {
            if let Some(c) = self.match_at(input, i) {
                return Some(c);
            }
            i += 1;
            if self.flags.unicode
                && i < input.len()
                && (0xDC00..0xE000).contains(&input[i])
                && (0xD800..0xDC00).contains(&input[i - 1])
            {
                i += 1;
            }
        }
        None
    }
}

impl<'a> Matcher<'a> {
    /// The code point at `pos` (forwards) and its length in units.
    fn char_at(&self, pos: usize) -> Option<(u32, usize)> {
        let u = *self.input.get(pos)?;
        if self.prog.flags.unicode && (0xD800..0xDC00).contains(&u) {
            if let Some(&lo) = self.input.get(pos + 1) {
                if (0xDC00..0xE000).contains(&lo) {
                    return Some((0x10000 + (((u as u32) - 0xD800) << 10) + (lo as u32 - 0xDC00), 2));
                }
            }
        }
        Some((u as u32, 1))
    }

    /// The code point ending at `pos` (backwards).
    fn char_before(&self, pos: usize) -> Option<(u32, usize)> {
        if pos == 0 {
            return None;
        }
        let u = self.input[pos - 1];
        if self.prog.flags.unicode && (0xDC00..0xE000).contains(&u) && pos >= 2 {
            let hi = self.input[pos - 2];
            if (0xD800..0xDC00).contains(&hi) {
                return Some((0x10000 + ((hi as u32 - 0xD800) << 10) + (u as u32 - 0xDC00), 2));
            }
        }
        Some((u as u32, 1))
    }

    fn canon(&self, c: u32) -> u32 {
        if !self.prog.flags.ignore_case {
            c
        } else if self.prog.flags.unicode {
            simple_fold(c)
        } else {
            simple_upper(c)
        }
    }

    fn class_matches(&self, class: &Class, c: u32) -> bool {
        let ic = self.prog.flags.ignore_case;
        let test = |c: u32| {
            class.items.iter().any(|it| match it {
                ClassItem::Range(lo, hi) => (*lo..=*hi).contains(&c),
                ClassItem::Digit(n) => (0x30..=0x39).contains(&c) != *n,
                ClassItem::Word(n) => is_word(c) != *n,
                ClassItem::Space(n) => is_space(c) != *n,
                ClassItem::Prop(p, n) => {
                    let ch = char::from_u32(c);
                    let r = match p {
                        Prop::Letter => ch.is_some_and(|x| x.is_alphabetic()),
                        Prop::Upper => ch.is_some_and(|x| x.is_uppercase()),
                        Prop::Lower => ch.is_some_and(|x| x.is_lowercase()),
                        Prop::Number => ch.is_some_and(|x| x.is_numeric()),
                        Prop::Digit => ch.is_some_and(|x| x.is_numeric()),
                        Prop::Alphabetic => ch.is_some_and(|x| x.is_alphabetic()),
                        Prop::WhiteSpace => ch.is_some_and(|x| x.is_whitespace()),
                        Prop::Punctuation => ch.is_some_and(|x| {
                            x.is_ascii_punctuation() || (!x.is_ascii() && !x.is_alphanumeric() && !x.is_whitespace())
                        }),
                        Prop::Any => true,
                        Prop::Ascii => c < 128,
                        Prop::Emoji => (0x1F300..=0x1FAFF).contains(&c) || (0x2600..=0x27BF).contains(&c),
                    };
                    r != *n
                }
            })
        };
        let mut hit = test(c);
        if !hit && ic {
            let alts = [simple_upper(c), simple_fold(c)];
            hit = alts.iter().any(|&a| a != c && test(a));
            if !hit {
                // A range like [a-z] against 'K'.
                if let Some(ch) = char::from_u32(c) {
                    for l in ch.to_lowercase().chain(ch.to_uppercase()) {
                        if test(l as u32) {
                            hit = true;
                            break;
                        }
                    }
                }
            }
        }
        hit != class.negate
    }

    fn word_at(&self, pos: usize) -> bool {
        pos < self.input.len() && is_word(self.input[pos] as u32)
    }

    /// Runs from `pc` at `pos`. In `backward` mode (lookbehind) characters
    /// are consumed right to left. Returns the end position on success.
    fn run(
        &mut self,
        pc: u32,
        pos: usize,
        caps: &mut Vec<usize>,
        marks: &mut Vec<usize>,
        backward: bool,
    ) -> Option<usize> {
        let mut stack: Vec<Thread> = Vec::new();
        let mut t = Thread { pc, pos, caps: core::mem::take(caps), marks: core::mem::take(marks) };
        let result = loop {
            self.steps += 1;
            if self.steps > MAX_STEPS {
                break None;
            }
            let ok = match self.prog.insts[t.pc as usize] {
                Inst::Match => {
                    break Some(t.pos);
                }
                Inst::Char(c) => {
                    let got = if backward { self.char_before(t.pos) } else { self.char_at(t.pos) };
                    match got {
                        Some((x, n)) if self.canon(x) == self.canon(c) => {
                            t.pos = if backward { t.pos - n } else { t.pos + n };
                            t.pc += 1;
                            true
                        }
                        _ => false,
                    }
                }
                Inst::Any => {
                    let got = if backward { self.char_before(t.pos) } else { self.char_at(t.pos) };
                    match got {
                        Some((x, n)) if self.prog.flags.dot_all || !is_line_term(x) => {
                            t.pos = if backward { t.pos - n } else { t.pos + n };
                            t.pc += 1;
                            true
                        }
                        _ => false,
                    }
                }
                Inst::Class(i) => {
                    let got = if backward { self.char_before(t.pos) } else { self.char_at(t.pos) };
                    match got {
                        Some((x, n)) if self.class_matches(&self.prog.classes[i as usize], x) => {
                            t.pos = if backward { t.pos - n } else { t.pos + n };
                            t.pc += 1;
                            true
                        }
                        _ => false,
                    }
                }
                Inst::Split(a, b) => {
                    let mut alt = t.clone();
                    alt.pc = b;
                    stack.push(alt);
                    t.pc = a;
                    true
                }
                Inst::Jmp(a) => {
                    t.pc = a;
                    true
                }
                Inst::Save(slot) => {
                    if slot >= u32::MAX - 1000 {
                        let g = (u32::MAX - slot) as usize;
                        if g * 2 + 1 < t.caps.len() {
                            t.caps[g * 2] = NONE;
                            t.caps[g * 2 + 1] = NONE;
                        }
                    } else {
                        t.caps[slot as usize] = t.pos;
                    }
                    t.pc += 1;
                    true
                }
                Inst::Start => {
                    let ok = t.pos == 0 || (self.prog.flags.multiline && is_line_term(self.input[t.pos - 1] as u32));
                    t.pc += 1;
                    ok
                }
                Inst::End => {
                    let ok = t.pos == self.input.len()
                        || (self.prog.flags.multiline && is_line_term(self.input[t.pos] as u32));
                    t.pc += 1;
                    ok
                }
                Inst::WordBoundary(want) => {
                    let a = t.pos > 0 && self.word_at(t.pos - 1);
                    let b = self.word_at(t.pos);
                    t.pc += 1;
                    (a != b) == want
                }
                Inst::Backref(g) => {
                    let (s, e) = (t.caps[g as usize * 2], t.caps[g as usize * 2 + 1]);
                    if s == NONE || e == NONE {
                        t.pc += 1;
                        true
                    } else {
                        let len = e - s;
                        let ok = if backward {
                            t.pos >= len
                                && (0..len).all(|i| {
                                    self.canon(self.input[s + i] as u32)
                                        == self.canon(self.input[t.pos - len + i] as u32)
                                })
                        } else {
                            t.pos + len <= self.input.len()
                                && (0..len).all(|i| {
                                    self.canon(self.input[s + i] as u32) == self.canon(self.input[t.pos + i] as u32)
                                })
                        };
                        if ok {
                            t.pos = if backward { t.pos - len } else { t.pos + len };
                            t.pc += 1;
                        }
                        ok
                    }
                }
                Inst::Look { ahead, negate, end } => {
                    let mut caps = t.caps.clone();
                    let mut marks = t.marks.clone();
                    let matched = self.run(t.pc + 1, t.pos, &mut caps, &mut marks, !ahead).is_some();
                    if self.steps > MAX_STEPS {
                        break None;
                    }
                    if matched != negate {
                        if !negate {
                            t.caps = caps;
                        }
                        t.pc = end;
                        true
                    } else {
                        false
                    }
                }
                Inst::Mark(m) => {
                    t.marks[m as usize] = t.pos;
                    t.pc += 1;
                    true
                }
                Inst::Progress(m) => {
                    t.pc += 1;
                    t.marks[m as usize] != t.pos
                }
            };
            if !ok {
                match stack.pop() {
                    Some(next) => t = next,
                    None => break None,
                }
            }
        };
        if result.is_some() {
            *caps = t.caps;
            *marks = t.marks;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn find(p: &str, f: &str, s: &str) -> Option<(usize, usize)> {
        let prog = compile(&u(p), f).unwrap();
        prog.exec(&u(s), 0).map(|c| c[0].unwrap())
    }

    fn groups(p: &str, f: &str, s: &str) -> Vec<Option<String>> {
        let prog = compile(&u(p), f).unwrap();
        let input = u(s);
        let c = prog.exec(&input, 0).unwrap();
        c.iter().map(|g| g.map(|(a, b)| String::from_utf16(&input[a..b]).unwrap())).collect()
    }

    #[test]
    fn basics() {
        assert_eq!(find("abc", "", "xxabcxx"), Some((2, 5)));
        assert_eq!(find("a+b*", "", "caaab"), Some((1, 5)));
        assert_eq!(find("a+?", "", "aaa"), Some((0, 1)));
        assert_eq!(find("^b", "m", "a\nb"), Some((2, 3)));
        assert_eq!(find("^b", "", "a\nb"), None);
        assert_eq!(find("\\bfoo\\b", "", "a foo b"), Some((2, 5)));
        assert_eq!(find("[a-c]+", "", "xxbcay"), Some((2, 5)));
        assert_eq!(find("[^a-c]+", "", "abxyc"), Some((2, 4)));
        assert_eq!(find("\\d{2,3}", "", "a12345"), Some((1, 4)));
        assert_eq!(find("ABC", "i", "xabc"), Some((1, 4)));
        assert_eq!(find("a.c", "", "a\nc"), None);
        assert_eq!(find("a.c", "s", "a\nc"), Some((0, 3)));
        assert_eq!(find("(a*)*b", "", "aaab"), Some((0, 4)));
        assert_eq!(find("x{", "", "x{"), Some((0, 2)));
        assert_eq!(find("(?<=\\$)\\d+", "", "cost: $42"), Some((7, 9)));
        assert_eq!(find("(?<!\\$)\\b\\d+", "", "$4 5"), Some((3, 4)));
        assert_eq!(find("foo(?=bar)", "", "foobaz foobar"), Some((7, 10)));
        assert_eq!(find("(?<=ab)c", "", "bc ac abc"), Some((8, 9)));
        assert_eq!(groups("(?<=(\\d)(\\d))x", "", "12x"), vec![Some("x".into()), Some("1".into()), Some("2".into())]);
        assert_eq!(find("foo(?!bar)", "", "foobar foobaz"), Some((7, 10)));
        assert_eq!(find("\\u{1F600}", "u", "x😀"), Some((1, 3)));
        assert_eq!(find("^.$", "u", "😀"), Some((0, 2)));
        assert_eq!(find("^.$", "", "😀"), None);
        assert_eq!(find("\\p{Lu}+", "u", "abcDEFg"), Some((3, 6)));
    }

    #[test]
    fn captures() {
        assert_eq!(
            groups("(\\w+)@(\\w+)\\.com", "", "mail bob@site.com now"),
            vec![Some("bob@site.com".into()), Some("bob".into()), Some("site".into())]
        );
        assert_eq!(groups("(a)|(b)", "", "b"), vec![Some("b".into()), None, Some("b".into())]);
        assert_eq!(groups("(\\w)\\1", "", "abccd"), vec![Some("cc".into()), Some("c".into())]);
        assert_eq!(groups("(?<y>\\d{4})-\\k<y>", "", "2024-2024"), vec![Some("2024-2024".into()), Some("2024".into())]);
        assert_eq!(
            groups("(z)((a+)?(b+)?(c))*", "", "zaacbbbcac"),
            vec![
                Some("zaacbbbcac".into()),
                Some("z".into()),
                Some("ac".into()),
                Some("a".into()),
                None,
                Some("c".into())
            ]
        );
        assert!(compile(&u("a)"), "").is_err());
        assert!(compile(&u("(?<n>a)\\k<m>"), "").is_err());
        assert!(compile(&u("a"), "gg").is_err());
        assert!(compile(&u("[b-a]"), "").is_err());
    }
}
