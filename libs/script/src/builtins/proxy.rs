//! Proxy objects (ECMA-262 §10.5): every internal method consults the
//! handler's trap, falling back to the target. The realm's object
//! operations dispatch here when they meet a proxy.

use crate::heap::ObjRef;
use crate::object::*;
use crate::realm::{Call, JsResult, PropDesc, Realm};
use crate::value::{PropKey, Value};
use alloc::boxed::Box;
use alloc::vec::Vec;

impl Realm {
    pub fn is_proxy(&self, o: ObjRef) -> bool {
        matches!(self.heap.get(o).kind, Kind::Proxy(_))
    }

    /// (target, handler), or a TypeError for a revoked proxy.
    fn proxy_parts(&mut self, o: ObjRef) -> Result<(ObjRef, ObjRef), Value> {
        match &self.heap.get(o).kind {
            Kind::Proxy(p) => match (p.target, p.handler) {
                (Some(t), Some(h)) => Ok((t, h)),
                _ => Err(self.type_error("Cannot perform operation on a revoked proxy")),
            },
            _ => Err(self.type_error("not a proxy")),
        }
    }

    /// The handler's trap, or None when it is undefined or null.
    fn trap(&mut self, handler: ObjRef, name: &str) -> Result<Option<Value>, Value> {
        let t = self.get_str(handler, name)?;
        if t.is_nullish() {
            return Ok(None);
        }
        if !self.is_callable(&t) {
            return Err(self.type_error(&alloc::format!("proxy trap '{name}' is not a function")));
        }
        Ok(Some(t))
    }

    pub(crate) fn proxy_get(&mut self, o: ObjRef, key: &PropKey, receiver: Value) -> JsResult {
        let (target, handler) = self.proxy_parts(o)?;
        match self.trap(handler, "get")? {
            Some(t) => {
                let k = key.to_value();
                self.call(&t, Value::Object(handler), &[Value::Object(target), k, receiver])
            }
            None => self.get(target, key, receiver),
        }
    }

    pub(crate) fn proxy_set(&mut self, o: ObjRef, key: PropKey, v: Value, receiver: Value) -> Result<bool, Value> {
        let (target, handler) = self.proxy_parts(o)?;
        match self.trap(handler, "set")? {
            Some(t) => {
                let k = key.to_value();
                let r = self.call(&t, Value::Object(handler), &[Value::Object(target), k, v, receiver])?;
                Ok(r.truthy())
            }
            None => self.set(target, key, v, receiver),
        }
    }

    pub(crate) fn proxy_has(&mut self, o: ObjRef, key: &PropKey) -> Result<bool, Value> {
        let (target, handler) = self.proxy_parts(o)?;
        match self.trap(handler, "has")? {
            Some(t) => {
                let k = key.to_value();
                Ok(self.call(&t, Value::Object(handler), &[Value::Object(target), k])?.truthy())
            }
            None => self.has_property(target, key),
        }
    }

    pub(crate) fn proxy_delete(&mut self, o: ObjRef, key: &PropKey) -> Result<bool, Value> {
        let (target, handler) = self.proxy_parts(o)?;
        match self.trap(handler, "deleteProperty")? {
            Some(t) => {
                let k = key.to_value();
                Ok(self.call(&t, Value::Object(handler), &[Value::Object(target), k])?.truthy())
            }
            None => self.delete(target, key),
        }
    }

    pub(crate) fn proxy_define(&mut self, o: ObjRef, key: PropKey, desc: PropDesc) -> Result<bool, Value> {
        let (target, handler) = self.proxy_parts(o)?;
        match self.trap(handler, "defineProperty")? {
            Some(t) => {
                let k = key.to_value();
                let d = super::object::from_desc(self, &desc);
                Ok(self.call(&t, Value::Object(handler), &[Value::Object(target), k, d])?.truthy())
            }
            None => self.define_own(target, key, desc),
        }
    }

    /// [[GetOwnProperty]] that can run proxy traps.
    pub fn get_own_property_v(&mut self, o: ObjRef, key: &PropKey) -> Result<Option<Prop>, Value> {
        if !self.is_proxy(o) {
            return Ok(self.get_own_property(o, key));
        }
        let (target, handler) = self.proxy_parts(o)?;
        match self.trap(handler, "getOwnPropertyDescriptor")? {
            Some(t) => {
                let k = key.to_value();
                let d = self.call(&t, Value::Object(handler), &[Value::Object(target), k])?;
                if d.is_undefined() {
                    return Ok(None);
                }
                if !matches!(d, Value::Object(_)) {
                    return Err(
                        self.type_error("getOwnPropertyDescriptor trap returned neither an object nor undefined")
                    );
                }
                let desc = super::object::to_prop_desc(self, &d)?;
                Ok(Some(prop_from_desc(&desc)))
            }
            None => self.get_own_property_v(target, key),
        }
    }

    /// [[OwnPropertyKeys]] that can run proxy traps.
    pub fn own_keys_v(&mut self, o: ObjRef) -> Result<Vec<PropKey>, Value> {
        if !self.is_proxy(o) {
            return Ok(self.own_keys(o));
        }
        let (target, handler) = self.proxy_parts(o)?;
        match self.trap(handler, "ownKeys")? {
            Some(t) => {
                let list = self.call(&t, Value::Object(handler), &[Value::Object(target)])?;
                if !matches!(list, Value::Object(_)) {
                    return Err(self.type_error("ownKeys trap result must be an object"));
                }
                let mut out = Vec::new();
                for v in self.list_from_array_like(&list)? {
                    match v {
                        Value::String(s) => out.push(PropKey::Str(s)),
                        Value::Symbol(s) => out.push(PropKey::Sym(s)),
                        _ => return Err(self.type_error("ownKeys trap result must contain only strings and symbols")),
                    }
                }
                Ok(out)
            }
            None => self.own_keys_v(target),
        }
    }

    pub fn get_prototype_of_v(&mut self, o: ObjRef) -> Result<Option<ObjRef>, Value> {
        if !self.is_proxy(o) {
            return Ok(self.heap.get(o).proto);
        }
        let (target, handler) = self.proxy_parts(o)?;
        match self.trap(handler, "getPrototypeOf")? {
            Some(t) => match self.call(&t, Value::Object(handler), &[Value::Object(target)])? {
                Value::Object(p) => Ok(Some(p)),
                Value::Null => Ok(None),
                _ => Err(self.type_error("getPrototypeOf trap returned neither object nor null")),
            },
            None => self.get_prototype_of_v(target),
        }
    }

    pub fn set_prototype_of_v(&mut self, o: ObjRef, proto: Option<ObjRef>) -> Result<bool, Value> {
        if !self.is_proxy(o) {
            return Ok(self.set_proto(o, proto));
        }
        let (target, handler) = self.proxy_parts(o)?;
        match self.trap(handler, "setPrototypeOf")? {
            Some(t) => {
                let p = proto.map(Value::Object).unwrap_or(Value::Null);
                Ok(self.call(&t, Value::Object(handler), &[Value::Object(target), p])?.truthy())
            }
            None => self.set_prototype_of_v(target, proto),
        }
    }

    pub fn is_extensible_v(&mut self, o: ObjRef) -> Result<bool, Value> {
        if !self.is_proxy(o) {
            return Ok(self.is_extensible(o));
        }
        let (target, handler) = self.proxy_parts(o)?;
        match self.trap(handler, "isExtensible")? {
            Some(t) => Ok(self.call(&t, Value::Object(handler), &[Value::Object(target)])?.truthy()),
            None => self.is_extensible_v(target),
        }
    }

    pub fn prevent_extensions_v(&mut self, o: ObjRef) -> Result<bool, Value> {
        if !self.is_proxy(o) {
            self.prevent_extensions(o);
            return Ok(true);
        }
        let (target, handler) = self.proxy_parts(o)?;
        match self.trap(handler, "preventExtensions")? {
            Some(t) => Ok(self.call(&t, Value::Object(handler), &[Value::Object(target)])?.truthy()),
            None => self.prevent_extensions_v(target),
        }
    }

    pub(crate) fn proxy_call(&mut self, o: ObjRef, this: Value, args: &[Value]) -> JsResult {
        let (target, handler) = self.proxy_parts(o)?;
        match self.trap(handler, "apply")? {
            Some(t) => {
                let a = self.array_from(args.to_vec());
                self.call(&t, Value::Object(handler), &[Value::Object(target), this, a])
            }
            None => self.call(&Value::Object(target), this, args),
        }
    }

    pub(crate) fn proxy_construct(&mut self, o: ObjRef, args: &[Value], new_target: Value) -> JsResult {
        let (target, handler) = self.proxy_parts(o)?;
        match self.trap(handler, "construct")? {
            Some(t) => {
                let a = self.array_from(args.to_vec());
                let r = self.call(&t, Value::Object(handler), &[Value::Object(target), a, new_target])?;
                if !matches!(r, Value::Object(_)) {
                    return Err(self.type_error("proxy [[Construct]] must return an object"));
                }
                Ok(r)
            }
            None => self.construct(&Value::Object(target), args, Some(new_target)),
        }
    }

    /// IsArray, looking through proxies.
    pub fn is_array_v(&mut self, v: &Value) -> Result<bool, Value> {
        let Value::Object(o) = v else { return Ok(false) };
        match &self.heap.get(*o).kind {
            Kind::Array(_) => Ok(true),
            Kind::Proxy(_) => {
                let (target, _) = self.proxy_parts(*o)?;
                self.is_array_v(&Value::Object(target))
            }
            _ => Ok(false),
        }
    }

    fn set_proto(&mut self, o: ObjRef, proto: Option<ObjRef>) -> bool {
        if self.heap.get(o).proto == proto {
            return true;
        }
        if !self.heap.get(o).extensible {
            return false;
        }
        // No cycles.
        let mut p = proto;
        while let Some(x) = p {
            if x == o {
                return false;
            }
            if self.is_proxy(x) {
                break;
            }
            p = self.heap.get(x).proto;
        }
        self.heap.get_mut(o).proto = proto;
        true
    }
}

fn prop_from_desc(d: &PropDesc) -> Prop {
    let mut flags = 0;
    if d.enumerable == Some(true) {
        flags |= ENUMERABLE;
    }
    if d.configurable == Some(true) {
        flags |= CONFIGURABLE;
    }
    if d.is_accessor() {
        Prop {
            slot: Slot::Accessor(d.get.clone().unwrap_or(Value::Undefined), d.set.clone().unwrap_or(Value::Undefined)),
            flags,
        }
    } else {
        if d.writable == Some(true) {
            flags |= WRITABLE;
        }
        Prop::data(d.value.clone().unwrap_or(Value::Undefined), flags)
    }
}

fn proxy_ctor(rt: &mut Realm, c: &Call) -> JsResult {
    if !c.is_construct() {
        return Err(rt.type_error("Constructor Proxy requires 'new'"));
    }
    make(rt, &c.arg(0), &c.arg(1))
}

fn make(rt: &mut Realm, target: &Value, handler: &Value) -> JsResult {
    let (Value::Object(t), Value::Object(h)) = (target, handler) else {
        return Err(rt.type_error("Cannot create proxy with a non-object as target or handler"));
    };
    let callable = rt.is_callable(target);
    let constructor = rt.is_constructor(target);
    let o = rt.alloc(Obj::new(
        None,
        Kind::Proxy(Box::new(ProxyData { target: Some(*t), handler: Some(*h), callable, constructor })),
    ));
    Ok(Value::Object(o))
}

fn revocable(rt: &mut Realm, c: &Call) -> JsResult {
    let p = make(rt, &c.arg(0), &c.arg(1))?;
    let revoke = rt.native_with("", 0, revoke, false, alloc::vec![p.clone()]);
    let out = rt.new_object();
    rt.define(out, "proxy", p, DEFAULT);
    rt.define(out, "revoke", Value::Object(revoke), DEFAULT);
    Ok(Value::Object(out))
}

fn revoke(rt: &mut Realm, c: &Call) -> JsResult {
    let slot = rt.native_slots(c.callee)[0].clone();
    if let Value::Object(p) = slot {
        if let Kind::Proxy(d) = &mut rt.heap.get_mut(p).kind {
            d.target = None;
            d.handler = None;
        }
    }
    if let Kind::Native(n) = &mut rt.heap.get_mut(c.callee).kind {
        n.slots[0] = Value::Null;
    }
    Ok(Value::Undefined)
}

pub fn init(rt: &mut Realm) {
    let ctor = rt.native("Proxy", 2, proxy_ctor, true);
    rt.method(ctor, "revocable", 2, revocable);
    rt.set_global("Proxy", Value::Object(ctor));
}
