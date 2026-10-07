//! The object heap: an arena of slots addressed by `ObjRef`, collected by
//! mark and sweep. Collection only happens at safe points chosen by the VM
//! (see `Realm::maybe_collect`), never inside an allocation, so Rust code
//! holding `ObjRef`s between safe points is always safe.

use crate::object::{Kind, Obj};
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ObjRef(pub u32);

pub struct Heap {
    slots: Vec<Option<Obj>>,
    free: Vec<u32>,
    live: usize,
    allocated_since: usize,
    threshold: usize,
    /// Allocations between collections, at least (lowered by tests to
    /// collect constantly).
    pub min_threshold: usize,
    pub collections: u32,
}

impl Default for Heap {
    fn default() -> Self {
        Heap {
            slots: Vec::new(),
            free: Vec::new(),
            live: 0,
            allocated_since: 0,
            threshold: 50_000,
            min_threshold: 50_000,
            collections: 0,
        }
    }
}

impl Heap {
    pub fn alloc(&mut self, obj: Obj) -> ObjRef {
        self.live += 1;
        self.allocated_since += 1;
        match self.free.pop() {
            Some(i) => {
                self.slots[i as usize] = Some(obj);
                ObjRef(i)
            }
            None => {
                self.slots.push(Some(obj));
                ObjRef(self.slots.len() as u32 - 1)
            }
        }
    }

    #[inline]
    pub fn get(&self, r: ObjRef) -> &Obj {
        self.slots[r.0 as usize].as_ref().expect("use of a collected object")
    }

    #[inline]
    pub fn get_mut(&mut self, r: ObjRef) -> &mut Obj {
        self.slots[r.0 as usize].as_mut().expect("use of a collected object")
    }

    pub fn live(&self) -> usize {
        self.live
    }

    /// Collect after every `n` allocations (tests use small values).
    pub fn set_pressure(&mut self, n: usize) {
        self.min_threshold = n;
        self.threshold = n;
    }

    pub fn wants_collection(&self) -> bool {
        self.allocated_since >= self.threshold
    }

    /// Marks from `roots`, then frees everything unreached. Weak maps and
    /// sets drop entries whose keys died (values are kept alive only through
    /// live keys: ephemerons).
    pub fn collect(&mut self, roots: Vec<crate::heap::ObjRef>) {
        let mut work = roots;
        let mut weak: Vec<ObjRef> = Vec::new();
        loop {
            while let Some(r) = work.pop() {
                let Some(obj) = self.slots[r.0 as usize].as_mut() else { continue };
                if obj.mark {
                    continue;
                }
                obj.mark = true;
                let obj = self.slots[r.0 as usize].as_ref().unwrap();
                if matches!(obj.kind, Kind::WeakMap(_)) {
                    weak.push(r);
                }
                obj.trace(&mut work);
            }
            // Ephemerons: a weak map's value is live if its key is.
            let mut more = false;
            for &w in &weak {
                if let Kind::WeakMap(m) = &self.get(w).kind {
                    for (k, v) in m.values() {
                        if self.get(*k).mark {
                            if let crate::value::Value::Object(o) = v {
                                if !self.get(*o).mark {
                                    work.push(*o);
                                    more = true;
                                }
                            }
                        }
                    }
                }
            }
            if !more {
                break;
            }
        }
        // Drop dead weak entries before their keys' slots are reused.
        let marked = |slots: &Vec<Option<Obj>>, r: ObjRef| slots[r.0 as usize].as_ref().is_some_and(|o| o.mark);
        let mut dead_keys: Vec<(usize, u32)> = Vec::new();
        for (i, s) in self.slots.iter().enumerate() {
            if let Some(o) = s {
                if !o.mark {
                    continue;
                }
                match &o.kind {
                    Kind::WeakMap(m) => {
                        for (id, (k, _)) in m.iter() {
                            if !marked(&self.slots, *k) {
                                dead_keys.push((i, *id));
                            }
                        }
                    }
                    Kind::WeakSet(m) => {
                        for (id, k) in m.iter() {
                            if !marked(&self.slots, *k) {
                                dead_keys.push((i, *id));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        for (i, id) in dead_keys {
            match &mut self.slots[i].as_mut().unwrap().kind {
                Kind::WeakMap(m) => {
                    m.remove(&id);
                }
                Kind::WeakSet(m) => {
                    m.remove(&id);
                }
                _ => {}
            }
        }
        let mut live = 0;
        for (i, s) in self.slots.iter_mut().enumerate() {
            match s {
                Some(o) if o.mark => {
                    o.mark = false;
                    live += 1;
                }
                Some(_) => {
                    *s = None;
                    self.free.push(i as u32);
                }
                None => {}
            }
        }
        self.live = live;
        self.allocated_since = 0;
        self.threshold = (live * 2).max(self.min_threshold);
        self.collections += 1;
    }
}
