const o = []; for (let i = 0; i < 20000; i++) o.push({ id: i, name: 'n' + i, tags: ['a', 'b'], ok: true });
const s = JSON.stringify(o); const back = JSON.parse(s); console.log(s.length, back.length);
