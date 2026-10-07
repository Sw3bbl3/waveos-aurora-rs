class P { constructor(x, y) { this.x = x; this.y = y; } len() { return this.x * this.x + this.y * this.y; } }
let t = 0; for (let i = 0; i < 500000; i++) { const p = new P(i, i + 1); t += p.len() % 13; } console.log(t);
