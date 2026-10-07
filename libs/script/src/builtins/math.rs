//! Math.

use super::*;

fn arg_n(rt: &mut Realm, c: &Call, i: usize) -> Result<f64, Value> {
    rt.to_number(&c.arg(i))
}

macro_rules! unary {
    ($name:ident, $f:expr) => {
        fn $name(rt: &mut Realm, c: &Call) -> JsResult {
            let x = arg_n(rt, c, 0)?;
            let f: fn(f64) -> f64 = $f;
            Ok(Value::Number(f(x)))
        }
    };
}

unary!(abs, |x| x.abs());
unary!(acos, libm::acos);
unary!(acosh, libm::acosh);
unary!(asin, libm::asin);
unary!(asinh, |x| if x == 0.0 { x } else { libm::asinh(x) });
unary!(atan, libm::atan);
unary!(atanh, libm::atanh);
unary!(cbrt, libm::cbrt);
unary!(ceil, libm::ceil);
unary!(cos, libm::cos);
unary!(cosh, libm::cosh);
unary!(exp, libm::exp);
unary!(expm1, libm::expm1);
unary!(floor, libm::floor);
unary!(fround, |x| x as f32 as f64);
unary!(log, libm::log);
unary!(log1p, libm::log1p);
unary!(log10, libm::log10);
unary!(log2, libm::log2);
unary!(sin, libm::sin);
unary!(sinh, libm::sinh);
unary!(sqrt, libm::sqrt);
unary!(tan, libm::tan);
unary!(tanh, libm::tanh);
unary!(trunc, libm::trunc);
unary!(sign, |x| if x.is_nan() || x == 0.0 {
    x
} else if x > 0.0 {
    1.0
} else {
    -1.0
});
unary!(round, |x| {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    if x > 0.0 && x < 0.5 {
        return 0.0;
    }
    if (-0.5..0.0).contains(&x) {
        return -0.0;
    }
    let f = libm::floor(x);
    if x - f >= 0.5 {
        f + 1.0
    } else {
        f
    }
});
unary!(clz32, |x| crate::numconv::to_uint32(x).leading_zeros() as f64);

fn atan2(rt: &mut Realm, c: &Call) -> JsResult {
    let y = arg_n(rt, c, 0)?;
    let x = arg_n(rt, c, 1)?;
    Ok(Value::Number(libm::atan2(y, x)))
}

fn pow(rt: &mut Realm, c: &Call) -> JsResult {
    let x = arg_n(rt, c, 0)?;
    let y = arg_n(rt, c, 1)?;
    Ok(Value::Number(crate::vm::js_pow(x, y)))
}

fn imul(rt: &mut Realm, c: &Call) -> JsResult {
    let a = rt.to_int32(&c.arg(0))?;
    let b = rt.to_int32(&c.arg(1))?;
    Ok(Value::Number(a.wrapping_mul(b) as f64))
}

fn max(rt: &mut Realm, c: &Call) -> JsResult {
    let mut r = f64::NEG_INFINITY;
    let mut nan = false;
    for a in &c.args {
        let x = rt.to_number(a)?;
        if x.is_nan() {
            nan = true;
        } else if x > r || (x == 0.0 && r == 0.0 && !x.is_sign_negative()) {
            r = x;
        }
    }
    Ok(Value::Number(if nan { f64::NAN } else { r }))
}

fn min(rt: &mut Realm, c: &Call) -> JsResult {
    let mut r = f64::INFINITY;
    let mut nan = false;
    for a in &c.args {
        let x = rt.to_number(a)?;
        if x.is_nan() {
            nan = true;
        } else if x < r || (x == 0.0 && r == 0.0 && x.is_sign_negative()) {
            r = x;
        }
    }
    Ok(Value::Number(if nan { f64::NAN } else { r }))
}

fn hypot(rt: &mut Realm, c: &Call) -> JsResult {
    let mut nums = Vec::new();
    for a in &c.args {
        nums.push(rt.to_number(a)?);
    }
    if nums.iter().any(|x| x.is_infinite()) {
        return Ok(Value::Number(f64::INFINITY));
    }
    if nums.iter().any(|x| x.is_nan()) {
        return Ok(Value::Number(f64::NAN));
    }
    let m = nums.iter().fold(0.0f64, |m, x| m.max(x.abs()));
    if m == 0.0 {
        return Ok(Value::Number(0.0));
    }
    let s: f64 = nums.iter().map(|x| (x / m) * (x / m)).sum();
    Ok(Value::Number(m * libm::sqrt(s)))
}

fn random(rt: &mut Realm, _c: &Call) -> JsResult {
    Ok(Value::Number(rt.random()))
}

pub fn init(rt: &mut Realm) {
    let m = rt.new_object();
    for (name, v) in [
        ("E", core::f64::consts::E),
        ("LN10", core::f64::consts::LN_10),
        ("LN2", core::f64::consts::LN_2),
        ("LOG10E", core::f64::consts::LOG10_E),
        ("LOG2E", core::f64::consts::LOG2_E),
        ("PI", core::f64::consts::PI),
        ("SQRT1_2", core::f64::consts::FRAC_1_SQRT_2),
        ("SQRT2", core::f64::consts::SQRT_2),
    ] {
        constant(rt, m, name, Value::Number(v));
    }
    for (name, len, f) in [
        ("abs", 1, abs as NativeFn),
        ("acos", 1, acos),
        ("acosh", 1, acosh),
        ("asin", 1, asin),
        ("asinh", 1, asinh),
        ("atan", 1, atan),
        ("atanh", 1, atanh),
        ("atan2", 2, atan2),
        ("cbrt", 1, cbrt),
        ("ceil", 1, ceil),
        ("clz32", 1, clz32),
        ("cos", 1, cos),
        ("cosh", 1, cosh),
        ("exp", 1, exp),
        ("expm1", 1, expm1),
        ("floor", 1, floor),
        ("fround", 1, fround),
        ("hypot", 2, hypot),
        ("imul", 2, imul),
        ("log", 1, log),
        ("log1p", 1, log1p),
        ("log10", 1, log10),
        ("log2", 1, log2),
        ("max", 2, max),
        ("min", 2, min),
        ("pow", 2, pow),
        ("random", 0, random),
        ("round", 1, round),
        ("sign", 1, sign),
        ("sin", 1, sin),
        ("sinh", 1, sinh),
        ("sqrt", 1, sqrt),
        ("tan", 1, tan),
        ("tanh", 1, tanh),
        ("trunc", 1, trunc),
    ] {
        rt.method(m, name, len, f);
    }
    to_string_tag(rt, m, "Math");
    rt.set_global("Math", Value::Object(m));
}
