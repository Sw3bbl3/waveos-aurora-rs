function* count(n) { for (let i = 0; i < n; i++) { const got = yield i; if (got) console.log("got", got); } return "done"; }
const g = count(3);
console.log(g.next(), g.next("hi"), g.next(), g.next(), g.next());
function* delegating() { const r = yield* count(2); console.log("inner returned", r); yield "after"; }
console.log([...delegating()]);
function* withFinally() { try { yield 1; yield 2; } finally { console.log("generator cleanup"); } }
const wf = withFinally(); console.log(wf.next()); console.log(wf.return(42)); console.log(wf.next());
function* thrower() { try { yield 1; } catch (e) { console.log("caught in gen", e); yield 2; } }
const t = thrower(); t.next(); console.log(t.throw("boom"), t.next());
const fibs = { *[Symbol.iterator]() { let [a, b] = [0, 1]; while (true) { yield a; [a, b] = [b, a + b]; } } };
const first = []; for (const f of fibs) { if (f > 50) break; first.push(f); } console.log(first.join());
const [x, y, ...z] = count(5); console.log(x, y, z);
const it = [1, 2, 3][Symbol.iterator](); console.log(it.next(), [...it]);
console.log(Object.prototype.toString.call(count(1)), typeof count(1)[Symbol.iterator]);
function* inf() { let i = 0; while (true) yield i++; }
const taken = []; for (const v of inf()) { taken.push(v); if (taken.length === 5) break; } console.log(taken);
console.log(Array.from(new Map([["k", "v"]]).entries()), [...new Set("hello")].join(""));
