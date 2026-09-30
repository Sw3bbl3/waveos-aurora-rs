//! CSS values: lengths (with calc/min/max/clamp) and colours.

use alloc::string::String;
use alloc::vec::Vec;

/// Rounds to the nearest integer (no_std has no `f32::round`).
pub fn round_i(x: f32) -> i32 {
    if x >= 0.0 {
        (x + 0.5) as i32
    } else {
        (x - 0.5) as i32
    }
}

/// A length before layout: percentages need the containing block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Len {
    Auto,
    /// `pct`% of the reference plus `px` (plain lengths have pct = 0).
    Calc(f32, f32),
}

impl Len {
    pub const ZERO: Len = Len::Calc(0.0, 0.0);

    pub fn px(v: f32) -> Len {
        Len::Calc(0.0, v)
    }

    pub fn is_auto(self) -> bool {
        self == Len::Auto
    }

    /// Resolves against `base` (the containing block's width, usually).
    pub fn resolve(self, base: i32) -> Option<i32> {
        match self {
            Len::Auto => None,
            Len::Calc(p, px) => Some(round_i(p * base as f32 / 100.0 + px)),
        }
    }

    /// Resolves, with `auto` as 0.
    pub fn or_zero(self, base: i32) -> i32 {
        self.resolve(base).unwrap_or(0)
    }
}

/// What relative units resolve against.
#[derive(Clone, Copy, Debug)]
pub struct Units {
    /// The element's font size (for em), px.
    pub em: f32,
    pub rem: f32,
    pub vw: f32,
    pub vh: f32,
}

/// Parses a number with an optional unit at the start of `s`: (value, unit, bytes used).
fn number(s: &str) -> Option<(f32, &str, usize)> {
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    let digits = i;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
        i += 1;
    }
    if i == digits {
        return None;
    }
    // Exponent.
    if i + 1 < b.len() && (b[i] == b'e' || b[i] == b'E') && (b[i + 1].is_ascii_digit() || b[i + 1] == b'-') {
        i += 2;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    let v: f32 = s[..i].parse().ok()?;
    let us = i;
    while i < b.len() && (b[i].is_ascii_alphabetic() || b[i] == b'%') {
        i += 1;
    }
    Some((v, &s[us..i], i))
}

/// One dimension in `unit`, as (percent, px).
fn dimension(v: f32, unit: &str, u: &Units) -> Option<(f32, f32)> {
    Some(match unit.to_ascii_lowercase().as_str() {
        "px" | "" => (0.0, v),
        "%" => (v, 0.0),
        "em" => (0.0, v * u.em),
        "rem" => (0.0, v * u.rem),
        "ex" | "ch" => (0.0, v * u.em * 0.5),
        "pt" => (0.0, v * 4.0 / 3.0),
        "pc" => (0.0, v * 16.0),
        "in" => (0.0, v * 96.0),
        "cm" => (0.0, v * 96.0 / 2.54),
        "mm" => (0.0, v * 96.0 / 25.4),
        "q" => (0.0, v * 96.0 / 101.6),
        "vw" | "svw" | "lvw" | "dvw" | "vi" => (0.0, v * u.vw / 100.0),
        "vh" | "svh" | "lvh" | "dvh" | "vb" => (0.0, v * u.vh / 100.0),
        "vmin" => (0.0, v * u.vw.min(u.vh) / 100.0),
        "vmax" => (0.0, v * u.vw.max(u.vh) / 100.0),
        _ => return None,
    })
}

/// A length: "12px", "1.5em", "50%", "0", "auto", "calc(100% - 2rem)", "min(…)".
pub fn length(s: &str, u: &Units) -> Option<Len> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("auto") {
        return Some(Len::Auto);
    }
    let (p, px) = expr(s, u)?;
    Some(Len::Calc(p, px))
}

/// A length that must not be a percentage (borders, font sizes use their own rules).
pub fn px_length(s: &str, u: &Units) -> Option<f32> {
    match length(s, u)? {
        Len::Calc(p, px) if p == 0.0 => Some(px),
        _ => None,
    }
}

/// A calc() operand or whole expression: (percent, px).
fn expr(s: &str, u: &Units) -> Option<(f32, f32)> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    for f in ["calc(", "-webkit-calc(", "min(", "max(", "clamp("] {
        if lower.starts_with(f) && s.ends_with(')') {
            let inner = &s[f.len()..s.len() - 1];
            return match f {
                "calc(" | "-webkit-calc(" => sum(inner, u),
                _ => {
                    // Percentages are taken against the viewport width here.
                    let vals: Vec<f32> = crate::css::split_top(inner, b',')
                        .iter()
                        .map(|a| expr(a, u).map(|(p, px)| p * u.vw / 100.0 + px))
                        .collect::<Option<Vec<f32>>>()?;
                    let v = match f {
                        "min(" => vals.iter().copied().fold(f32::MAX, f32::min),
                        "max(" => vals.iter().copied().fold(f32::MIN, f32::max),
                        _ if vals.len() == 3 => vals[1].clamp(vals[0], vals[2].max(vals[0])),
                        _ => return None,
                    };
                    Some((0.0, v))
                }
            };
        }
    }
    if s.starts_with('(') && s.ends_with(')') {
        return sum(&s[1..s.len() - 1], u);
    }
    if lower.starts_with("var(") {
        return None;
    }
    let (v, unit, used) = number(s)?;
    if used != s.len() {
        return None;
    }
    if unit.is_empty() && v != 0.0 {
        // Unitless numbers are only lengths when zero (quirks aside).
        return Some((0.0, v));
    }
    dimension(v, unit, u)
}

/// A calc() sum of products.
fn sum(s: &str, u: &Units) -> Option<(f32, f32)> {
    // Split into terms on + and - that are surrounded by whitespace.
    let b = s.as_bytes();
    let mut terms: Vec<(f32, &str)> = Vec::new();
    let (mut depth, mut start, mut sign) = (0, 0, 1.0f32);
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b'+' | b'-'
                if depth == 0
                    && i > 0
                    && b[i - 1].is_ascii_whitespace()
                    && b.get(i + 1).is_some_and(|c| c.is_ascii_whitespace()) =>
            {
                terms.push((sign, &s[start..i]));
                sign = if b[i] == b'+' { 1.0 } else { -1.0 };
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    terms.push((sign, &s[start..]));
    let mut total = (0.0, 0.0);
    for (sign, t) in terms {
        let (p, px) = product(t.trim(), u)?;
        total.0 += sign * p;
        total.1 += sign * px;
    }
    Some(total)
}

fn product(s: &str, u: &Units) -> Option<(f32, f32)> {
    let parts: Vec<&str> = crate::css::split_top(s, b'*');
    if parts.len() > 1 {
        let mut acc = (0.0f32, 1.0f32);
        let mut scalar = 1.0f32;
        let mut have_dim = false;
        for p in parts {
            let p = p.trim();
            if let Ok(n) = p.parse::<f32>() {
                scalar *= n;
            } else {
                acc = product(p, u)?;
                have_dim = true;
            }
        }
        return Some(if have_dim { (acc.0 * scalar, acc.1 * scalar) } else { (0.0, scalar) });
    }
    let parts: Vec<&str> = crate::css::split_top(s, b'/');
    if parts.len() == 2 {
        let d: f32 = parts[1].trim().parse().ok()?;
        if d == 0.0 {
            return None;
        }
        let (p, px) = expr(parts[0].trim(), u)?;
        return Some((p / d, px / d));
    }
    expr(s, u)
}

// ------------------------------------------------------------------- colours

/// Parses a colour to 0xAARRGGBB. `current` is the currentColor.
pub fn color(s: &str, current: u32) -> Option<u32> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    if let Some(hex) = lower.strip_prefix('#') {
        let d: Vec<u32> = hex.chars().map(|c| c.to_digit(16)).collect::<Option<Vec<u32>>>()?;
        let (r, g, b, a) = match d.len() {
            3 => (d[0] * 17, d[1] * 17, d[2] * 17, 255),
            4 => (d[0] * 17, d[1] * 17, d[2] * 17, d[3] * 17),
            6 => (d[0] << 4 | d[1], d[2] << 4 | d[3], d[4] << 4 | d[5], 255),
            8 => (d[0] << 4 | d[1], d[2] << 4 | d[3], d[4] << 4 | d[5], d[6] << 4 | d[7]),
            _ => return None,
        };
        return Some(a << 24 | r << 16 | g << 8 | b);
    }
    if lower == "transparent" {
        return Some(0);
    }
    if lower == "currentcolor" {
        return Some(current);
    }
    if let Some(open) = lower.find('(') {
        let func = &lower[..open];
        let args = lower[open + 1..].trim_end_matches(')');
        let parts: Vec<&str> =
            args.split(|c: char| c == ',' || c == '/' || c.is_whitespace()).filter(|p| !p.is_empty()).collect();
        if parts.len() < 3 {
            return None;
        }
        let alpha = match parts.get(3) {
            Some(a) => channel(a, 1.0)?,
            None => 1.0,
        };
        let (r, g, b) = match func {
            "rgb" | "rgba" => (channel(parts[0], 255.0)?, channel(parts[1], 255.0)?, channel(parts[2], 255.0)?),
            "hsl" | "hsla" => {
                let h = parts[0].trim_end_matches("deg").parse::<f32>().ok()?;
                let sat = channel(parts[1], 1.0)?;
                let l = channel(parts[2], 1.0)?;
                hsl(h, sat, l)
            }
            _ => return None,
        };
        let c = |v: f32| (v.clamp(0.0, 255.0) + 0.5) as u32;
        return Some(c(alpha * 255.0) << 24 | c(r) << 16 | c(g) << 8 | c(b));
    }
    named(&lower).map(|c| 0xFF00_0000 | c)
}

/// A number or percentage scaled so 100% = `full`.
fn channel(s: &str, full: f32) -> Option<f32> {
    if let Some(p) = s.strip_suffix('%') {
        return Some(p.parse::<f32>().ok()? * full / 100.0);
    }
    if s == "none" {
        return Some(0.0);
    }
    s.parse().ok()
}

fn hsl(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let h = (h - 360.0 * ((h / 360.0) as i32 as f32 - if h < 0.0 { 1.0 } else { 0.0 })) / 360.0;
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let f = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        255.0
            * if t < 1.0 / 6.0 {
                p + (q - p) * 6.0 * t
            } else if t < 0.5 {
                q
            } else if t < 2.0 / 3.0 {
                p + (q - p) * (2.0 / 3.0 - t) * 6.0
            } else {
                p
            }
    };
    (f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0))
}

fn named(n: &str) -> Option<u32> {
    COLORS.binary_search_by(|(k, _)| k.cmp(&n)).ok().map(|i| COLORS[i].1)
}

/// The CSS named colours, sorted for binary search.
const COLORS: &[(&str, u32)] = &[
    ("aliceblue", 0xf0f8ff),
    ("antiquewhite", 0xfaebd7),
    ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4),
    ("black", 0x000000),
    ("blanchedalmond", 0xffebcd),
    ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2),
    ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887),
    ("cadetblue", 0x5f9ea0),
    ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e),
    ("coral", 0xff7f50),
    ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc),
    ("crimson", 0xdc143c),
    ("cyan", 0x00ffff),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xa9a9a9),
    ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b),
    ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00),
    ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a),
    ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f),
    ("darkslategrey", 0x2f4f4f),
    ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("floralwhite", 0xfffaf0),
    ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700),
    ("goldenrod", 0xdaa520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xadff2f),
    ("grey", 0x808080),
    ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4),
    ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd),
    ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2),
    ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90),
    ("lightgrey", 0xd3d3d3),
    ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a),
    ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899),
    ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0),
    ("lime", 0x00ff00),
    ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd),
    ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db),
    ("mediumseagreen", 0x3cb371),
    ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead),
    ("navy", 0x000080),
    ("oldlace", 0xfdf5e6),
    ("olive", 0x808000),
    ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500),
    ("orangered", 0xff4500),
    ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa),
    ("palegreen", 0x98fb98),
    ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5),
    ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xff0000),
    ("rosybrown", 0xbc8f8f),
    ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072),
    ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0),
    ("skyblue", 0x87ceeb),
    ("slateblue", 0x6a5acd),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("teal", 0x008080),
    ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee),
    ("wheat", 0xf5deb3),
    ("white", 0xffffff),
    ("whitesmoke", 0xf5f5f5),
    ("yellow", 0xffff00),
    ("yellowgreen", 0x9acd32),
];

/// Substitutes var(--name, fallback) using `lookup`.
pub fn substitute_vars(value: &str, lookup: &dyn Fn(&str) -> Option<String>, depth: u32) -> Option<String> {
    if !value.contains("var(") {
        return Some(String::from(value));
    }
    if depth > 8 {
        return None;
    }
    let mut out = String::new();
    let mut rest = value;
    while let Some(p) = rest.find("var(") {
        out.push_str(&rest[..p]);
        let after = &rest[p + 4..];
        // Find the matching ')'.
        let mut depth_p = 1;
        let mut end = after.len();
        for (i, c) in after.char_indices() {
            match c {
                '(' => depth_p += 1,
                ')' => {
                    depth_p -= 1;
                    if depth_p == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let inner = &after[..end];
        let (name, fallback) = match inner.find(',') {
            Some(c) => (inner[..c].trim(), Some(inner[c + 1..].trim())),
            None => (inner.trim(), None),
        };
        let v = match lookup(name) {
            Some(v) => v,
            None => String::from(fallback?),
        };
        out.push_str(&substitute_vars(&v, lookup, depth + 1)?);
        rest = &after[(end + 1).min(after.len())..];
    }
    out.push_str(rest);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const U: Units = Units { em: 20.0, rem: 16.0, vw: 1000.0, vh: 800.0 };

    #[test]
    fn lengths() {
        assert_eq!(length("12px", &U), Some(Len::px(12.0)));
        assert_eq!(length("1.5em", &U), Some(Len::px(30.0)));
        assert_eq!(length("2rem", &U), Some(Len::px(32.0)));
        assert_eq!(length("50%", &U), Some(Len::Calc(50.0, 0.0)));
        assert_eq!(length("10vw", &U), Some(Len::px(100.0)));
        assert_eq!(length("auto", &U), Some(Len::Auto));
        assert_eq!(length("calc(100% - 2rem)", &U), Some(Len::Calc(100.0, -32.0)));
        assert_eq!(length("calc((100% - 10px) / 2)", &U), Some(Len::Calc(50.0, -5.0)));
        assert_eq!(length("calc(2 * 1em + 4px)", &U), Some(Len::px(44.0)));
        assert_eq!(length("min(100%, 600px)", &U), Some(Len::px(600.0)));
        assert_eq!(length("clamp(10px, 5vw, 30px)", &U), Some(Len::px(30.0)));
        assert_eq!(length("bogus", &U), None);
        assert_eq!(Len::Calc(100.0, -32.0).resolve(500), Some(468));
    }

    #[test]
    fn colors() {
        assert_eq!(color("#fff", 0), Some(0xFFFF_FFFF));
        assert_eq!(color("#1a2b3c", 0), Some(0xFF1A_2B3C));
        assert_eq!(color("#1a2b3c80", 0), Some(0x801A_2B3C));
        assert_eq!(color("rgb(255, 0, 0)", 0), Some(0xFFFF_0000));
        assert_eq!(color("rgba(0,0,0,.5)", 0), Some(0x8000_0000));
        assert_eq!(color("rgb(0 128 255 / 50%)", 0), Some(0x8000_80FF));
        assert_eq!(color("hsl(120, 100%, 25%)", 0), Some(0xFF00_8000));
        assert_eq!(color("RebeccaPurple", 0), Some(0xFF66_3399));
        assert_eq!(color("transparent", 0), Some(0));
        assert_eq!(color("currentColor", 0xFF12_3456), Some(0xFF12_3456));
        assert_eq!(color("nope", 0), None);
        // The table must stay sorted.
        assert!(COLORS.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn vars() {
        let look = |n: &str| match n {
            "--a" => Some(String::from("10px")),
            "--b" => Some(String::from("var(--a) solid")),
            _ => None,
        };
        assert_eq!(substitute_vars("var(--b) red", &look, 0).unwrap(), "10px solid red");
        assert_eq!(substitute_vars("var(--x, var(--a))", &look, 0).unwrap(), "10px");
        assert_eq!(substitute_vars("var(--x)", &look, 0), None);
    }
}
