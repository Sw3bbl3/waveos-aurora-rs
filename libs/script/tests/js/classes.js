class Animal {
  static count = 0;
  #secret = "hidden";
  name;
  constructor(name) { this.name = name; Animal.count++; }
  speak() { return `${this.name} makes a sound`; }
  get secret() { return this.#secret; }
  set secret(v) { this.#secret = v; }
  static create(n) { return new this(n); }
  #privMethod() { return "private " + this.name; }
  callPriv() { return this.#privMethod(); }
  static { this.initialized = true; }
  static isAnimal(x) { return #secret in x; }
}
class Dog extends Animal {
  constructor(name) { super(name); this.kind = "dog"; }
  speak() { return super.speak() + " (woof)"; }
  toString() { return `Dog(${this.name})`; }
}
const d = new Dog("Rex");
console.log(d.speak(), String(d), d.secret, Animal.count, Animal.initialized);
d.secret = "changed"; console.log(d.secret, d.callPriv());
console.log(d instanceof Dog, d instanceof Animal, Object.getPrototypeOf(Dog) === Animal);
console.log(Dog.create("Fido").speak(), Animal.count, Animal.isAnimal(d), Animal.isAnimal({}));
console.log(typeof Dog, Dog.name, Animal.prototype.constructor === Animal);
try { Dog(); } catch (e) { console.log(e instanceof TypeError); }
try { d.secret2 = 1; console.log(Object.keys(d)); } catch (e) { console.log("err"); }
class Point { constructor(x, y) { Object.assign(this, { x, y }); } [Symbol.iterator]() { return [this.x, this.y][Symbol.iterator](); } }
console.log([...new Point(1, 2)]);
const Mixin = Base => class extends Base { mixed() { return "mixed " + this.name; } };
class M extends Mixin(Animal) {}
console.log(new M("m").mixed());
class E extends Error { constructor(m) { super(m); this.name = "E"; } }
const e = new E("custom"); console.log(e.message, e.name, e instanceof Error, String(e));
const obj = { __proto__: { inherited: 1 }, own: 2, ["comp" + "uted"]: 3, method() { return super.inherited; } };
console.log(obj.inherited, obj.computed, obj.method(), Object.keys(obj));
const acc = { _v: 1, get v() { return this._v; }, set v(x) { this._v = x * 2; } }; acc.v = 5; console.log(acc.v);
console.log(class {}.name, (class Named {}).name, (() => {}).name);
const named = { fn: function () {}, arrow: () => {} }; console.log(named.fn.name, named.arrow.name);
