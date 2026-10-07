const log = console.log;
log("start");
setTimeoutLike(() => log("never runs (no timers in core)"));
function setTimeoutLike() {}
Promise.resolve().then(() => log("micro 1")).then(() => log("micro 3"));
queueMicrotask(() => log("micro 2"));
(async function () {
  log("async body");
  await null;
  log("after await");
  const results = await Promise.all([1, Promise.resolve(2), new Promise(r => r(3))]);
  log("all", results);
  const settled = await Promise.allSettled([Promise.reject(new Error("no")), 5]);
  log("settled", settled.map(s => s.status));
  try { await Promise.reject(new TypeError("bad")); } catch (e) { log("caught", e.name, e.message); }
  log("race", await Promise.race([new Promise(() => {}), Promise.resolve("fast")]));
  log("any", await Promise.any([Promise.reject(1), Promise.resolve(2)]));
  try { await Promise.any([Promise.reject(1)]); } catch (e) { log(e.constructor.name, e.errors); }
  async function* agen() { yield 1; await null; yield 2; }
  const got = []; for await (const v of agen()) got.push(v); log("for await", got);
  for await (const v of [Promise.resolve("a"), "b"]) log("sync iterable", v);
  const p = new Promise((res, rej) => rej("x")).catch(e => "recovered " + e).finally(() => log("finally"));
  log(await p);
})();
log("end");
