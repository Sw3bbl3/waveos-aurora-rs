let parts = []; for (let i = 0; i < 100000; i++) parts.push('item' + i);
const s = parts.join(','); let c = 0; for (const p of s.split(',')) if (p.endsWith('7')) c++;
const m = {}; for (let i = 0; i < 100000; i++) m['k' + (i % 1000)] = i; console.log(c, Object.keys(m).length);
