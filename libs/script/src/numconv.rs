//! Numbers to strings and back, exactly as ECMAScript specifies.
//!
//! Shortest round-trip digits come from `core`'s float formatting; the
//! fixed/exponential/precision forms use the double's exact decimal
//! expansion (a small bignum), so `(1.005).toFixed(2)` is `"1.00"`, as in
//! every browser.

use crate::value::trim_units;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// Number::toString(10).
pub fn to_string(x: f64) -> String {
    if x.is_nan() {
        return String::from("NaN");
    }
    if x == 0.0 {
        return String::from("0");
    }
    if x.is_infinite() {
        return String::from(if x > 0.0 { "Infinity" } else { "-Infinity" });
    }
    let (digits, n) = shortest(x.abs());
    let mut out = String::new();
    if x < 0.0 {
        out.push('-');
    }
    format_with(&mut out, &digits, n);
    out
}

/// The shortest digits `d` and exponent `n` with x = 0.d × 10ⁿ.
pub fn shortest(x: f64) -> (Vec<u8>, i32) {
    let s = format!("{:e}", x);
    let (mant, exp) = s.split_once('e').unwrap();
    let digits: Vec<u8> = mant.bytes().filter(|b| b.is_ascii_digit()).map(|b| b - b'0').collect();
    let e: i32 = exp.parse().unwrap();
    (digits, e + 1)
}

fn format_with(out: &mut String, digits: &[u8], n: i32) {
    let k = digits.len() as i32;
    let push = |out: &mut String, d: &[u8]| {
        for &c in d {
            out.push((b'0' + c) as char);
        }
    };
    if k <= n && n <= 21 {
        push(out, digits);
        for _ in 0..n - k {
            out.push('0');
        }
    } else if 0 < n && n <= 21 {
        push(out, &digits[..n as usize]);
        out.push('.');
        push(out, &digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        for _ in 0..-n {
            out.push('0');
        }
        push(out, digits);
    } else {
        push(out, &digits[..1]);
        if k > 1 {
            out.push('.');
            push(out, &digits[1..]);
        }
        out.push('e');
        out.push(if n > 0 { '+' } else { '-' });
        out.push_str(&format!("{}", (n - 1).abs()));
    }
}

// ---------------------------------------------------------------- bignum

/// A little-endian arbitrary-precision unsigned integer.
#[derive(Clone)]
struct Big(Vec<u32>);

impl Big {
    fn from_u64(n: u64) -> Big {
        let mut b = Big(vec![n as u32, (n >> 32) as u32]);
        b.trim();
        b
    }

    fn trim(&mut self) {
        while self.0.len() > 1 && *self.0.last().unwrap() == 0 {
            self.0.pop();
        }
    }

    fn is_zero(&self) -> bool {
        self.0.iter().all(|&l| l == 0)
    }

    fn mul_small(&mut self, m: u32) {
        let mut carry = 0u64;
        for l in self.0.iter_mut() {
            let v = *l as u64 * m as u64 + carry;
            *l = v as u32;
            carry = v >> 32;
        }
        if carry > 0 {
            self.0.push(carry as u32);
        }
    }

    fn shl(&mut self, bits: u32) {
        let words = (bits / 32) as usize;
        let bits = bits % 32;
        if bits > 0 {
            let mut carry = 0u32;
            for l in self.0.iter_mut() {
                let v = (*l << bits) | carry;
                carry = *l >> (32 - bits);
                *l = v;
            }
            if carry > 0 {
                self.0.push(carry);
            }
        }
        for _ in 0..words {
            self.0.insert(0, 0);
        }
    }

    /// Divides in place, returning the remainder.
    fn div_small(&mut self, d: u32) -> u32 {
        let mut rem = 0u64;
        for l in self.0.iter_mut().rev() {
            let v = (rem << 32) | *l as u64;
            *l = (v / d as u64) as u32;
            rem = v % d as u64;
        }
        self.trim();
        rem as u32
    }

    /// Digits in base `radix`, most significant first.
    fn digits(mut self, radix: u32) -> Vec<u8> {
        if self.is_zero() {
            return vec![0];
        }
        let mut out = Vec::new();
        while !self.is_zero() {
            out.push(self.div_small(radix) as u8);
        }
        out.reverse();
        out
    }
}

/// x (positive, finite) = mantissa × 2^exp.
fn decompose(x: f64) -> (u64, i32) {
    let bits = x.to_bits();
    let e = ((bits >> 52) & 0x7FF) as i32;
    let f = bits & ((1u64 << 52) - 1);
    if e == 0 {
        (f, -1074)
    } else {
        (f | (1u64 << 52), e - 1075)
    }
}

/// The exact decimal value of positive finite x: digits × 10^exp.
fn exact(x: f64) -> (Vec<u8>, i32) {
    let (m, e) = decompose(x);
    let mut big = Big::from_u64(m);
    if e >= 0 {
        big.shl(e as u32);
        (big.digits(10), 0)
    } else {
        for _ in 0..-e {
            big.mul_small(5);
        }
        (big.digits(10), e)
    }
}

/// Rounds `digits` (most significant first) to `keep` digits, half up.
/// Returns the digits and whether a carry added a new leading digit.
fn round_to(digits: &[u8], keep: usize) -> (Vec<u8>, bool) {
    if keep >= digits.len() {
        let mut d = digits.to_vec();
        d.resize(keep, 0);
        return (d, false);
    }
    let mut d = digits[..keep].to_vec();
    if digits[keep] >= 5 {
        let mut i = keep;
        loop {
            if i == 0 {
                d.insert(0, 1);
                return (d, true);
            }
            i -= 1;
            if d[i] == 9 {
                d[i] = 0;
            } else {
                d[i] += 1;
                break;
            }
        }
    }
    (d, false)
}

fn digits_str(d: &[u8]) -> String {
    d.iter().map(|&c| (b'0' + c) as char).collect()
}

/// Number.prototype.toFixed for 0 ≤ f ≤ 100 and |x| < 1e21.
pub fn to_fixed(x: f64, f: usize) -> String {
    let neg = x < 0.0;
    let x = x.abs();
    let mut s = if x == 0.0 {
        let mut s = String::from("0");
        if f > 0 {
            s.push('.');
            s.extend(core::iter::repeat_n('0', f));
        }
        s
    } else {
        let (digits, e10) = exact(x);
        // n = round(x × 10^f) = digits × 10^(e10 + f)
        let shift = e10 + f as i32;
        let n: Vec<u8> = if shift >= 0 {
            let mut d = digits;
            d.extend(core::iter::repeat_n(0, shift as usize));
            d
        } else {
            let drop = (-shift) as usize;
            if drop > digits.len() {
                vec![0]
            } else {
                let keep = digits.len() - drop;
                if keep == 0 {
                    vec![if digits[0] >= 5 { 1 } else { 0 }]
                } else {
                    round_to(&digits, keep).0
                }
            }
        };
        let mut m = digits_str(&n);
        if f > 0 {
            if m.len() <= f {
                let pad = f + 1 - m.len();
                m = format!("{}{}", "0".repeat(pad), m);
            }
            let p = m.len() - f;
            m.insert(p, '.');
        }
        m
    };
    if neg {
        s.insert(0, '-');
    }
    s
}

/// Number.prototype.toExponential; `f` None means "as many digits as needed".
pub fn to_exponential(x: f64, f: Option<usize>) -> String {
    let neg = x < 0.0;
    let x = x.abs();
    let (d, e) = if x == 0.0 {
        (vec![0; f.unwrap_or(0) + 1], 0)
    } else {
        match f {
            None => {
                let (d, n) = shortest(x);
                (d, n - 1)
            }
            Some(f) => {
                let (digits, e10) = exact(x);
                let lead = digits.len() as i32 - 1 + e10;
                let (d, carry) = round_to(&digits, f + 1);
                let mut d = d;
                if carry {
                    d.pop();
                }
                (d, lead + carry as i32)
            }
        }
    };
    let mut s = String::new();
    if neg {
        s.push('-');
    }
    s.push((b'0' + d[0]) as char);
    if d.len() > 1 {
        s.push('.');
        s.push_str(&digits_str(&d[1..]));
    }
    s.push('e');
    s.push(if e >= 0 { '+' } else { '-' });
    s.push_str(&format!("{}", e.abs()));
    s
}

/// Number.prototype.toPrecision for 1 ≤ p ≤ 100.
pub fn to_precision(x: f64, p: usize) -> String {
    let neg = x < 0.0;
    let x = x.abs();
    let (d, e) = if x == 0.0 {
        (vec![0; p], 0)
    } else {
        let (digits, e10) = exact(x);
        let lead = digits.len() as i32 - 1 + e10;
        let (mut d, carry) = round_to(&digits, p);
        if carry {
            d.pop();
        }
        (d, lead + carry as i32)
    };
    let mut s = String::new();
    if neg {
        s.push('-');
    }
    if e < -6 || e >= p as i32 {
        s.push((b'0' + d[0]) as char);
        if p > 1 {
            s.push('.');
            s.push_str(&digits_str(&d[1..]));
        }
        s.push('e');
        s.push(if e >= 0 { '+' } else { '-' });
        s.push_str(&format!("{}", e.abs()));
    } else if e == p as i32 - 1 {
        s.push_str(&digits_str(&d));
    } else if e >= 0 {
        s.push_str(&digits_str(&d[..e as usize + 1]));
        s.push('.');
        s.push_str(&digits_str(&d[e as usize + 1..]));
    } else {
        s.push_str("0.");
        s.push_str(&"0".repeat((-(e + 1)) as usize));
        s.push_str(&digits_str(&d));
    }
    s
}

const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

fn next_double(x: f64) -> f64 {
    f64::from_bits(x.to_bits() + 1)
}

/// Number.prototype.toString(radix) for radix ≠ 10: V8's algorithm, digit
/// for digit (the de facto standard).
pub fn to_radix(x: f64, radix: u32) -> String {
    if x.is_nan() {
        return String::from("NaN");
    }
    if x.is_infinite() {
        return String::from(if x > 0.0 { "Infinity" } else { "-Infinity" });
    }
    if x == 0.0 {
        return String::from("0");
    }
    let neg = x < 0.0;
    let value = x.abs();
    let r = radix as f64;
    let mut integer = libm::floor(value);
    let mut fraction = value - integer;
    let mut delta = 0.5 * (next_double(value) - value);
    delta = delta.max(next_double(0.0));
    let mut frac: Vec<u8> = Vec::new();
    if fraction >= delta {
        loop {
            fraction *= r;
            delta *= r;
            let digit = fraction as u8;
            frac.push(digit);
            fraction -= digit as f64;
            if (fraction > 0.5 || (fraction == 0.5 && (digit & 1) == 1)) && fraction + delta > 1.0 {
                loop {
                    match frac.pop() {
                        None => {
                            integer += 1.0;
                            break;
                        }
                        Some(d) if (d as u32) + 1 < radix => {
                            frac.push(d + 1);
                            break;
                        }
                        Some(_) => {}
                    }
                }
                break;
            }
            if fraction < delta {
                break;
            }
        }
    }
    // Integer digits; those beyond the double's precision are zeros.
    let mut int_digits: Vec<u8> = Vec::new();
    while decompose(integer / r).1 > 0 {
        integer /= r;
        int_digits.push(0);
    }
    loop {
        let rem = libm::fmod(integer, r);
        int_digits.push(rem as u8);
        integer = (integer - rem) / r;
        if integer <= 0.0 {
            break;
        }
    }
    let mut s = String::new();
    if neg {
        s.push('-');
    }
    for &d in int_digits.iter().rev() {
        s.push(DIGITS[d as usize] as char);
    }
    if !frac.is_empty() {
        s.push('.');
        for d in frac {
            s.push(DIGITS[d as usize] as char);
        }
    }
    s
}

// ---------------------------------------------------------------- parsing

fn digit_value(c: u16) -> Option<u32> {
    let c = char::from_u32(c as u32)?;
    c.to_digit(36)
}

/// The length of the longest StrDecimalLiteral prefix (without sign), or 0.
fn decimal_prefix(u: &[u16]) -> usize {
    let is_d = |i: usize| i < u.len() && (b'0' as u16..=b'9' as u16).contains(&u[i]);
    let mut i = 0;
    let mut any = false;
    while is_d(i) {
        i += 1;
        any = true;
    }
    if i < u.len() && u[i] == b'.' as u16 {
        let mut j = i + 1;
        let mut frac = false;
        while is_d(j) {
            j += 1;
            frac = true;
        }
        if any || frac {
            i = j;
            any = true;
        }
    }
    if !any {
        return 0;
    }
    if i < u.len() && (u[i] == b'e' as u16 || u[i] == b'E' as u16) {
        let mut j = i + 1;
        if j < u.len() && (u[j] == b'+' as u16 || u[j] == b'-' as u16) {
            j += 1;
        }
        if is_d(j) {
            while is_d(j) {
                j += 1;
            }
            i = j;
        }
    }
    i
}

fn parse_ascii(u: &[u16]) -> f64 {
    let s: String = u.iter().map(|&c| c as u8 as char).collect();
    s.parse::<f64>().unwrap_or(f64::NAN)
}

fn starts_with(u: &[u16], s: &str) -> bool {
    u.len() >= s.len() && u.iter().zip(s.bytes()).all(|(&a, b)| a == b as u16)
}

/// StringToNumber (the `Number(string)` conversion).
pub fn parse(units: &[u16]) -> f64 {
    let u = trim_units(units, true, true);
    if u.is_empty() {
        return 0.0;
    }
    if u.len() > 2 && u[0] == b'0' as u16 {
        let radix = match u[1] {
            0x78 | 0x58 => 16,
            0x6F | 0x4F => 8,
            0x62 | 0x42 => 2,
            _ => 0,
        };
        if radix != 0 {
            let mut n = 0.0f64;
            for &c in &u[2..] {
                match digit_value(c) {
                    Some(d) if d < radix => n = n * radix as f64 + d as f64,
                    _ => return f64::NAN,
                }
            }
            return n;
        }
    }
    let (neg, body) = match u[0] {
        0x2B => (false, &u[1..]),
        0x2D => (true, &u[1..]),
        _ => (false, u),
    };
    let v = if starts_with(body, "Infinity") && body.len() == 8 {
        f64::INFINITY
    } else {
        let n = decimal_prefix(body);
        if n == 0 || n != body.len() {
            return f64::NAN;
        }
        parse_ascii(body)
    };
    if neg {
        -v
    } else {
        v
    }
}

/// The global parseFloat.
pub fn parse_float(units: &[u16]) -> f64 {
    let u = trim_units(units, true, false);
    let (neg, body) = match u.first() {
        Some(0x2B) => (false, &u[1..]),
        Some(0x2D) => (true, &u[1..]),
        _ => (false, u),
    };
    let v = if starts_with(body, "Infinity") {
        f64::INFINITY
    } else {
        let n = decimal_prefix(body);
        if n == 0 {
            return f64::NAN;
        }
        parse_ascii(&body[..n])
    };
    if neg {
        -v
    } else {
        v
    }
}

/// The global parseInt; `radix` 0 means "detect".
pub fn parse_int(units: &[u16], radix: i32) -> f64 {
    let mut u = trim_units(units, true, false);
    let mut neg = false;
    if let Some(&c) = u.first() {
        if c == b'-' as u16 || c == b'+' as u16 {
            neg = c == b'-' as u16;
            u = &u[1..];
        }
    }
    let mut radix = radix;
    let mut strip = true;
    if radix != 0 {
        if !(2..=36).contains(&radix) {
            return f64::NAN;
        }
        if radix != 16 {
            strip = false;
        }
    } else {
        radix = 10;
    }
    if strip && u.len() >= 2 && u[0] == b'0' as u16 && (u[1] == b'x' as u16 || u[1] == b'X' as u16) {
        u = &u[2..];
        radix = 16;
    }
    let end = u.iter().position(|&c| digit_value(c).is_none_or(|d| d >= radix as u32)).unwrap_or(u.len());
    let digits = &u[..end];
    if digits.is_empty() {
        return f64::NAN;
    }
    let v = if radix == 10 {
        parse_ascii(digits)
    } else {
        let mut n = 0.0f64;
        for &c in digits {
            n = n * radix as f64 + digit_value(c).unwrap() as f64;
        }
        n
    };
    if neg {
        -v
    } else {
        v
    }
}

/// ToInt32.
pub fn to_int32(n: f64) -> i32 {
    to_uint32(n) as i32
}

/// ToUint32.
pub fn to_uint32(n: f64) -> u32 {
    if !n.is_finite() || n == 0.0 {
        return 0;
    }
    if n.abs() < 2147483648.0 * 2.0 && n == libm::trunc(n) {
        return (n as i64) as u32;
    }
    let t = libm::trunc(n);
    let m = libm::fmod(t, 4294967296.0);
    let m = if m < 0.0 { m + 4294967296.0 } else { m };
    m as u64 as u32
}

/// ToIntegerOrInfinity.
pub fn to_integer(n: f64) -> f64 {
    if n.is_nan() {
        0.0
    } else {
        libm::trunc(n) + 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn shortest_forms() {
        for (x, s) in [
            (1.0, "1"),
            (0.1, "0.1"),
            (123.456, "123.456"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (1.5e-7, "1.5e-7"),
            (0.000001, "0.000001"),
            (-0.0, "0"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (0.1 + 0.2, "0.30000000000000004"),
            (123e-20, "1.23e-18"),
        ] {
            assert_eq!(to_string(x), s, "{x}");
        }
    }

    #[test]
    fn fixed_precision_exponential() {
        assert_eq!(to_fixed(1.005, 2), "1.00");
        assert_eq!(to_fixed(1.45, 1), "1.4");
        assert_eq!(to_fixed(0.5, 0), "1");
        assert_eq!(to_fixed(2.5, 0), "3");
        assert_eq!(to_fixed(1000000000000000128.0, 0), "1000000000000000128");
        assert_eq!(to_fixed(0.000001, 2), "0.00");
        assert_eq!(to_fixed(-1.5, 0), "-2");
        assert_eq!(to_fixed(0.0, 2), "0.00");
        assert_eq!(to_fixed(123.456, 10), "123.4560000000");
        assert_eq!(to_precision(123.456, 4), "123.5");
        assert_eq!(to_precision(0.000123, 2), "0.00012");
        assert_eq!(to_precision(123456.0, 2), "1.2e+5");
        assert_eq!(to_precision(1e21, 3), "1.00e+21");
        assert_eq!(to_precision(0.0, 3), "0.00");
        assert_eq!(to_exponential(123456.0, Some(2)), "1.23e+5");
        assert_eq!(to_exponential(0.00015, None), "1.5e-4");
        assert_eq!(to_exponential(0.0, Some(2)), "0.00e+0");
        assert_eq!(to_exponential(9.99, Some(1)), "1.0e+1");
    }

    #[test]
    fn radix() {
        assert_eq!(to_radix(255.0, 16), "ff");
        assert_eq!(to_radix(-255.0, 2), "-11111111");
        assert_eq!(to_radix(0.5, 2), "0.1");
        assert_eq!(to_radix(3.14, 16), "3.23d70a3d70a3e");
        assert_eq!(to_radix(1e21, 36), "5v1j4f4ds7c000");
        assert_eq!(to_radix(0.1, 3), "0.0022002200220022002200220022002201");
        assert_eq!(to_radix(1.0 / 3.0, 2), "0.010101010101010101010101010101010101010101010101010101");
    }

    #[test]
    fn parsing() {
        assert_eq!(parse(&u("  42 ")), 42.0);
        assert_eq!(parse(&u("")), 0.0);
        assert_eq!(parse(&u("0x1F")), 31.0);
        assert_eq!(parse(&u("-Infinity")), f64::NEG_INFINITY);
        assert!(parse(&u("inf")).is_nan());
        assert!(parse(&u("1e")).is_nan());
        assert_eq!(parse(&u(".5")), 0.5);
        assert_eq!(parse(&u("5.")), 5.0);
        assert!(parse(&u("-0x10")).is_nan());
        assert_eq!(parse_float(&u("3.14abc")), 3.14);
        assert_eq!(parse_float(&u("1e3x")), 1000.0);
        assert_eq!(parse_int(&u("0x10"), 0), 16.0);
        assert_eq!(parse_int(&u("08"), 0), 8.0);
        assert_eq!(parse_int(&u("  -12px"), 10), -12.0);
        assert!(parse_int(&u("z"), 10).is_nan());
        assert_eq!(parse_int(&u("z"), 36), 35.0);
        assert_eq!(to_int32(4294967296.0 + 5.0), 5);
        assert_eq!(to_int32(2147483648.0), -2147483648);
        assert_eq!(to_uint32(-1.0), 4294967295);
    }
}
