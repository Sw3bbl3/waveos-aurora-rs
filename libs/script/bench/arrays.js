const a = Array.from({ length: 200000 }, (_, i) => i);
const r = a.map(x => x * 2).filter(x => x % 3 === 0).reduce((s, x) => s + x, 0);
const b = a.slice().sort((x, y) => y - x); console.log(r, b[0]);
