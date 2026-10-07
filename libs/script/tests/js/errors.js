const log = console.log;
const cases = [
  () => undefinedThing,
  () => null.x,
  () => undefined.y.z,
  () => { let fn; fn(); },
  () => { const obj = {}; obj.nope(); },
  () => { const obj = { a: {} }; new obj.a.b(); },
  () => { const c = 1; c = 2; },
  () => new (class { constructor() { throw new RangeError("range!"); } })(),
  () => JSON.parse("{bad"),
  () => [].reduce((a, b) => a),
  () => new Array(-1),
  () => Symbol() + "",
  () => { "use strict"; undeclared = 1; },
  () => (1).toFixed(101),
  () => decodeURIComponent("%"),
  () => { throw { custom: true }; },
  () => new Function("return (")(),
  () => Object.defineProperty(Object.freeze({}), "x", { value: 1 }),
  () => { class A { #p; static get(o) { return o.#p; } } return A.get({}); },
];
// Engines word some messages differently; for these only the type is compared.
const nameOnly = new Set([8, 16]);
cases.forEach((f, i) => {
  try { f(); log("no error"); } catch (e) {
    if (!(e instanceof Error)) log(JSON.stringify(e));
    else log(nameOnly.has(i) ? e.name : `${e.name}: ${e.message}`);
  }
});
const err = new Error("with cause", { cause: "root" }); log(err.cause, err instanceof Error, Object.keys(err));
log(new TypeError("t").toString(), Error("no new").message, new AggregateError([1], "agg").errors);
try { try { throw new Error("inner"); } finally { log("finally first"); } } catch (e) { log("then outer", e.message); }
function deep(n) { return n === 0 ? 0 : 1 + deep(n - 1); } log(deep(1000));
try { (function r() { r(); })(); } catch (e) { log(e instanceof RangeError); }
