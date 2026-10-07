// Scoping, closures, hoisting and TDZ.
var log = console.log;
log(typeof hoisted, hoisted());
function hoisted() { return "hoisted"; }
log(typeof v1); var v1 = 1;
try { tdz; let tdz = 1; } catch (e) { log(e.constructor.name, e.message); }
{ let x = 1; { let x = 2; log(x); } log(x); }
const counters = [];
for (let i = 0; i < 3; i++) { counters.push(() => i * 10); }
log(counters.map(f => f()).join());
var vs = [];
for (var j = 0; j < 3; j++) { vs.push(() => j); }
log(vs.map(f => f()).join());
function outer() { let n = 0; return { inc: () => ++n, get: () => n }; }
const c = outer(); c.inc(); c.inc(); log(c.get());
function shadow(a) { { let a = 5; } return a; } log(shadow(3));
const fact = function f(n) { return n <= 1 ? 1 : n * f(n - 1); }; log(fact(10));
if (true) { function inBlock() { return "annex b"; } } log(inBlock());
switch (2) { case 1: let sw = "one"; break; case 2: try { sw = "two"; } catch (e) { log("switch TDZ", e.name); } }
log((function () { return typeof arguments; })(), (function () { return arguments.length; })(1, 2, 3));
log((() => { try { return "try"; } finally { log("finally runs"); } })());
function f2() { for (const x of [1, 2, 3]) { try { if (x === 2) return x; } finally { log("cleanup", x); } } } log(f2());
let k = 0; outerLoop: while (true) { do { k++; if (k > 3) break outerLoop; } while (true); } log(k);
log([1, 2, 3].reduce((a, b) => a + b), (x => y => x + y)(2)(3));
log(this === undefined ? "undefined this" : typeof this);
const objThis = { name: "o", reg() { return this.name; }, arrow: () => typeof this };
log(objThis.reg(), objThis.arrow());
log(void 0, typeof null, typeof function () {}, typeof Symbol());
const tgt = {}; const keyName = "k"; [tgt[keyName], tgt.other] = [1, 2]; ({ a: tgt["x" + 1] } = { a: 3 }); log(JSON.stringify(tgt));
