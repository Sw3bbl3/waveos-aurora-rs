// Web APIs written in JavaScript, run in every page's realm before its
// scripts. MutationObserver is live; the intersection and resize observers
// report once (asynchronously) and never again: enough for lazy loading and
// feature checks, not live tracking.
(function (global) {
  "use strict";
  const define = (name, value) =>
    Object.defineProperty(global, name, { value, writable: true, configurable: true, enumerable: false });
  const later = f => setTimeout(f, 0);

  // CharacterData is a real parent of Text, so libraries can patch its
  // prototype and existing/new text wrappers see the same operations.
  const characterText = node => {
    if (!(node instanceof CharacterData)) throw new TypeError("Illegal CharacterData invocation");
    return node.data;
  };
  const characterOffset = (text, offset) => {
    offset = Number(offset) >>> 0;
    if (offset > text.length) {
      const error = new Error("Offset exceeds data length"); error.name = "IndexSizeError"; throw error;
    }
    return offset;
  };
  Object.defineProperty(CharacterData.prototype, "length", {
    configurable: true, enumerable: true, get() { return characterText(this).length; }
  });
  Object.defineProperties(CharacterData.prototype, {
    substringData: { configurable: true, writable: true, value(offset, count) {
      const text = characterText(this); offset = characterOffset(text, offset);
      return text.slice(offset, offset + (Number(count) >>> 0));
    }},
    appendData: { configurable: true, writable: true, value(data) { this.data = characterText(this) + String(data); }},
    insertData: { configurable: true, writable: true, value(offset, data) { this.replaceData(offset, 0, data); }},
    deleteData: { configurable: true, writable: true, value(offset, count) { this.replaceData(offset, count, ""); }},
    replaceData: { configurable: true, writable: true, value(offset, count, data) {
      const text = characterText(this); offset = characterOffset(text, offset);
      this.data = text.slice(0, offset) + String(data) + text.slice(offset + (Number(count) >>> 0));
    }}
  });
  Object.defineProperty(Text.prototype, "splitText", { configurable: true, writable: true, value(offset) {
    const text = characterText(this); offset = characterOffset(text, offset);
    const tail = document.createTextNode(text.slice(offset));
    this.data = text.slice(0, offset);
    if (this.parentNode) this.parentNode.insertBefore(tail, this.nextSibling);
    return tail;
  }});

  // DOMException, with the legacy codes libraries still compare against.
  const exceptionCodes = {
    IndexSizeError: 1, HierarchyRequestError: 3, WrongDocumentError: 4, InvalidCharacterError: 5,
    NoModificationAllowedError: 7, NotFoundError: 8, NotSupportedError: 9, InvalidStateError: 11,
    SyntaxError: 12, InvalidModificationError: 13, NamespaceError: 14, InvalidAccessError: 15,
    TypeMismatchError: 17, SecurityError: 18, NetworkError: 19, AbortError: 20, URLMismatchError: 21,
    QuotaExceededError: 22, TimeoutError: 23, InvalidNodeTypeError: 24, DataCloneError: 25,
  };
  class DOMException extends Error {
    constructor(message = "", name = "Error") {
      super(String(message));
      Object.defineProperty(this, "name", { value: String(name), configurable: true, writable: true });
    }
    get code() { return exceptionCodes[this.name] || 0; }
  }
  for (const [name, code] of Object.entries(exceptionCodes)) {
    const constant = name.replace(/Error$/, "").replace(/([a-z])([A-Z])/g, "$1_$2").toUpperCase() + "_ERR";
    Object.defineProperty(DOMException, constant, { value: code });
    Object.defineProperty(DOMException.prototype, constant, { value: code });
  }
  define("DOMException", DOMException);

  // NodeFilter, TreeWalker and NodeIterator (DOM standard §6), on the tree API.
  const ACCEPT = 1, REJECT = 2, SKIP = 3;
  function NodeFilter() { throw new TypeError("Illegal constructor"); }
  for (const [name, value] of Object.entries({
    FILTER_ACCEPT: 1, FILTER_REJECT: 2, FILTER_SKIP: 3, SHOW_ALL: 0xFFFFFFFF, SHOW_ELEMENT: 0x1,
    SHOW_ATTRIBUTE: 0x2, SHOW_TEXT: 0x4, SHOW_CDATA_SECTION: 0x8, SHOW_ENTITY_REFERENCE: 0x10,
    SHOW_ENTITY: 0x20, SHOW_PROCESSING_INSTRUCTION: 0x40, SHOW_COMMENT: 0x80, SHOW_DOCUMENT: 0x100,
    SHOW_DOCUMENT_TYPE: 0x200, SHOW_DOCUMENT_FRAGMENT: 0x400, SHOW_NOTATION: 0x800,
  })) {
    Object.defineProperty(NodeFilter, name, { value, enumerable: true });
  }
  define("NodeFilter", NodeFilter);
  const traversal = new WeakMap();
  const state = t => {
    const s = traversal.get(t);
    if (!s) throw new TypeError("Illegal invocation");
    return s;
  };
  const filterNode = (s, node) => {
    if (s.active) throw new DOMException("The filter is already running", "InvalidStateError");
    if (!(s.whatToShow & (1 << (node.nodeType - 1)))) return SKIP;
    if (s.filter === null) return ACCEPT;
    s.active = true;
    try {
      const f = s.filter;
      const result = typeof f === "function" ? f(node) : f.acceptNode(node);
      return Number(result) & 0xFFFF;
    } finally {
      s.active = false;
    }
  };
  const newTraversal = (kind, root, whatToShow, filter) => {
    if (!(root instanceof Node)) throw new TypeError("parameter 1 is not of type 'Node'");
    const t = Object.create(kind.prototype);
    traversal.set(t, { root, whatToShow: whatToShow >>> 0, filter: filter ?? null, active: false,
      current: root, reference: root, before: true });
    return t;
  };
  function TreeWalker() { throw new TypeError("Illegal constructor"); }
  function NodeIterator() { throw new TypeError("Illegal constructor"); }
  const common = {
    root: { get() { return state(this).root; } },
    whatToShow: { get() { return state(this).whatToShow; } },
    filter: { get() { return state(this).filter; } },
  };
  const walkChildren = (s, first) => {
    let node = first ? s.current.firstChild : s.current.lastChild;
    while (node !== null) {
      const result = filterNode(s, node);
      if (result === ACCEPT) return (s.current = node);
      if (result === SKIP) {
        const child = first ? node.firstChild : node.lastChild;
        if (child !== null) { node = child; continue; }
      }
      while (node !== null) {
        const sibling = first ? node.nextSibling : node.previousSibling;
        if (sibling !== null) { node = sibling; break; }
        const parent = node.parentNode;
        if (parent === null || parent === s.root || parent === s.current) return null;
        node = parent;
      }
    }
    return null;
  };
  const walkSiblings = (s, next) => {
    let node = s.current;
    if (node === s.root) return null;
    for (;;) {
      let sibling = next ? node.nextSibling : node.previousSibling;
      while (sibling !== null) {
        node = sibling;
        const result = filterNode(s, node);
        if (result === ACCEPT) return (s.current = node);
        sibling = next ? node.firstChild : node.lastChild;
        if (result === REJECT || sibling === null) sibling = next ? node.nextSibling : node.previousSibling;
      }
      node = node.parentNode;
      if (node === null || node === s.root) return null;
      if (filterNode(s, node) === ACCEPT) return null;
    }
  };
  const method = value => ({ value, writable: true, configurable: true });
  Object.defineProperties(TreeWalker.prototype, {
    ...common,
    currentNode: {
      get() { return state(this).current; },
      set(node) {
        if (!(node instanceof Node)) throw new TypeError("parameter 1 is not of type 'Node'");
        state(this).current = node;
      },
    },
    parentNode: method(function () {
      const s = state(this);
      let node = s.current;
      while (node !== null && node !== s.root) {
        node = node.parentNode;
        if (node !== null && filterNode(s, node) === ACCEPT) return (s.current = node);
      }
      return null;
    }),
    firstChild: method(function () { return walkChildren(state(this), true); }),
    lastChild: method(function () { return walkChildren(state(this), false); }),
    previousSibling: method(function () { return walkSiblings(state(this), false); }),
    nextSibling: method(function () { return walkSiblings(state(this), true); }),
    previousNode: method(function () {
      const s = state(this);
      let node = s.current;
      while (node !== s.root) {
        let sibling = node.previousSibling;
        while (sibling !== null) {
          node = sibling;
          let result = filterNode(s, node);
          while (result !== REJECT && node.lastChild !== null) {
            node = node.lastChild;
            result = filterNode(s, node);
          }
          if (result === ACCEPT) return (s.current = node);
          sibling = node.previousSibling;
        }
        if (node === s.root || node.parentNode === null) return null;
        node = node.parentNode;
        if (filterNode(s, node) === ACCEPT) return (s.current = node);
      }
      return null;
    }),
    nextNode: method(function () {
      const s = state(this);
      let node = s.current, result = ACCEPT;
      for (;;) {
        while (result !== REJECT && node.firstChild !== null) {
          node = node.firstChild;
          result = filterNode(s, node);
          if (result === ACCEPT) return (s.current = node);
        }
        let following = null;
        for (let n = node; n !== null && n !== s.root; n = n.parentNode) {
          if (n.nextSibling !== null) { following = n.nextSibling; break; }
        }
        if (following === null) return null;
        node = following;
        result = filterNode(s, node);
        if (result === ACCEPT) return (s.current = node);
      }
    }),
    [Symbol.toStringTag]: { value: "TreeWalker", configurable: true },
  });
  const iterate = (s, next) => {
    let node = s.reference, before = s.before;
    for (;;) {
      if (next) {
        if (!before) {
          let n = node, found = node.firstChild;
          while (found === null && n !== null) {
            if (n === s.root) return null;
            found = n.nextSibling;
            n = n.parentNode;
          }
          if (found === null) return null;
          node = found;
        } else {
          before = false;
        }
      } else if (before) {
        if (node === s.root) return null;
        let p = node.previousSibling;
        if (p !== null) {
          while (p.lastChild !== null) p = p.lastChild;
          node = p;
        } else {
          node = node.parentNode;
          if (node === null) return null;
        }
      } else {
        before = true;
      }
      if (filterNode(s, node) === ACCEPT) break;
    }
    s.reference = node;
    s.before = before;
    return node;
  };
  Object.defineProperties(NodeIterator.prototype, {
    ...common,
    referenceNode: { get() { return state(this).reference; } },
    pointerBeforeReferenceNode: { get() { return state(this).before; } },
    nextNode: method(function () { return iterate(state(this), true); }),
    previousNode: method(function () { return iterate(state(this), false); }),
    detach: method(function () {}),
    [Symbol.toStringTag]: { value: "NodeIterator", configurable: true },
  });
  for (const [name, f] of Object.entries({ TreeWalker, NodeIterator })) {
    Object.defineProperty(f.prototype, "constructor", { value: f, writable: true, configurable: true });
    define(name, f);
  }
  Object.defineProperties(Document.prototype, {
    createTreeWalker: method(function (root, whatToShow = 0xFFFFFFFF, filter = null) {
      return newTraversal(TreeWalker, root, whatToShow, filter);
    }),
    createNodeIterator: method(function (root, whatToShow = 0xFFFFFFFF, filter = null) {
      return newTraversal(NodeIterator, root, whatToShow, filter);
    }),
  });

  // MutationObserver: options are validated here; the browser records
  // changes and delivers them in a microtask.
  const observe = __nebula_observe, unobserve = __nebula_disconnect, takeRecords = __nebula_take_records;
  const observerCallbacks = new WeakMap();
  class MutationObserver {
    constructor(callback) {
      if (typeof callback !== "function") {
        throw new TypeError("Failed to construct 'MutationObserver': parameter 1 is not of type 'MutationCallback'.");
      }
      observerCallbacks.set(this, callback);
    }
    observe(target, options = {}) {
      if (!(target instanceof Node)) throw new TypeError("Failed to execute 'observe': parameter 1 is not of type 'Node'.");
      let { childList = false, attributes, characterData, subtree = false,
        attributeOldValue, characterDataOldValue, attributeFilter } = options;
      if ((attributeOldValue !== undefined || attributeFilter !== undefined) && attributes === undefined) attributes = true;
      if (characterDataOldValue !== undefined && characterData === undefined) characterData = true;
      const fail = why => { throw new TypeError("Failed to execute 'observe' on 'MutationObserver': " + why); };
      if (!childList && !attributes && !characterData) {
        fail("The options object must set at least one of 'attributes', 'characterData', or 'childList' to true.");
      }
      if (attributeOldValue && !attributes) fail("'attributeOldValue' requires 'attributes'.");
      if (attributeFilter !== undefined && !attributes) fail("'attributeFilter' requires 'attributes'.");
      if (characterDataOldValue && !characterData) fail("'characterDataOldValue' requires 'characterData'.");
      observe(this, observerCallbacks.get(this), target, {
        childList: !!childList, attributes: !!attributes, characterData: !!characterData, subtree: !!subtree,
        attributeOldValue: !!attributeOldValue, characterDataOldValue: !!characterDataOldValue,
        attributeFilter: attributeFilter === undefined ? null : Array.from(attributeFilter, String),
      });
    }
    disconnect() { unobserve(this); }
    takeRecords() { return takeRecords(this); }
  }

  class IntersectionObserver {
    constructor(callback, options = {}) {
      this._callback = callback;
      this._targets = new Set();
      this.root = options.root || null;
      this.rootMargin = options.rootMargin || "0px";
      this.thresholds = [].concat(options.threshold ?? 0);
    }
    observe(target) {
      if (this._targets.has(target)) return;
      this._targets.add(target);
      // Everything counts as visible: lazy-loaded content loads.
      later(() => {
        if (!this._targets.has(target)) return;
        const r = target.getBoundingClientRect();
        this._callback([{ target, isIntersecting: true, intersectionRatio: 1, time: performance.now(),
          boundingClientRect: r, intersectionRect: r, rootBounds: null }], this);
      });
    }
    unobserve(target) { this._targets.delete(target); }
    disconnect() { this._targets.clear(); }
    takeRecords() { return []; }
  }

  class ResizeObserver {
    constructor(callback) { this._callback = callback; this._targets = new Set(); }
    observe(target) {
      this._targets.add(target);
      later(() => {
        if (!this._targets.has(target)) return;
        const r = target.getBoundingClientRect();
        const size = [{ inlineSize: r.width, blockSize: r.height }];
        this._callback([{ target, contentRect: r, borderBoxSize: size, contentBoxSize: size }], this);
      });
    }
    unobserve(target) { this._targets.delete(target); }
    disconnect() { this._targets.clear(); }
  }

  class PerformanceObserver {
    constructor(callback) { this._callback = callback; }
    observe() {}
    disconnect() {}
    takeRecords() { return []; }
  }
  PerformanceObserver.supportedEntryTypes = [];

  for (const [name, value] of Object.entries({ MutationObserver, IntersectionObserver, ResizeObserver, PerformanceObserver })) {
    define(name, value);
  }

  const marks = [];
  Object.assign(performance, {
    mark(name) { const m = { name, entryType: "mark", startTime: performance.now(), duration: 0 }; marks.push(m); return m; },
    measure(name) { return { name, entryType: "measure", startTime: 0, duration: 0 }; },
    getEntriesByType(type) { return type === "mark" ? marks.slice() : []; },
    getEntriesByName(name) { return marks.filter(m => m.name === name); },
    getEntries() { return marks.slice(); },
    clearMarks() { marks.length = 0; },
    clearMeasures() {},
    timing: { navigationStart: performance.timeOrigin },
    navigation: { type: 0 },
  });

  define("requestIdleCallback", cb => setTimeout(() => cb({ didTimeout: false, timeRemaining: () => 50 }), 1));
  define("cancelIdleCallback", id => clearTimeout(id));

  // URLSearchParams and URL.
  const decode = s => decodeURIComponent(s.replace(/\+/g, " "));
  const encode = s => encodeURIComponent(s).replace(/%20/g, "+");
  class URLSearchParams {
    constructor(init = "") {
      this._list = [];
      if (typeof init === "string") {
        for (const part of init.replace(/^\?/, "").split("&")) {
          if (!part) continue;
          const i = part.indexOf("=");
          this._list.push(i < 0 ? [decode(part), ""] : [decode(part.slice(0, i)), decode(part.slice(i + 1))]);
        }
      } else if (init && typeof init[Symbol.iterator] === "function") {
        for (const [k, v] of init) this._list.push([String(k), String(v)]);
      } else if (init && typeof init === "object") {
        for (const k of Object.keys(init)) this._list.push([k, String(init[k])]);
      }
    }
    get size() { return this._list.length; }
    append(k, v) { this._list.push([String(k), String(v)]); this._changed(); }
    delete(k) { this._list = this._list.filter(e => e[0] !== k); this._changed(); }
    get(k) { const e = this._list.find(e => e[0] === k); return e ? e[1] : null; }
    getAll(k) { return this._list.filter(e => e[0] === k).map(e => e[1]); }
    has(k) { return this._list.some(e => e[0] === k); }
    set(k, v) {
      const i = this._list.findIndex(e => e[0] === k);
      if (i < 0) this._list.push([String(k), String(v)]);
      else { this._list[i][1] = String(v); this._list = this._list.filter((e, j) => j <= i || e[0] !== k); }
      this._changed();
    }
    sort() { this._list.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0)); this._changed(); }
    forEach(f, self) { for (const [k, v] of this._list) f.call(self, v, k, this); }
    keys() { return this._list.map(e => e[0])[Symbol.iterator](); }
    values() { return this._list.map(e => e[1])[Symbol.iterator](); }
    entries() { return this._list.map(e => [e[0], e[1]])[Symbol.iterator](); }
    [Symbol.iterator]() { return this.entries(); }
    toString() { return this._list.map(([k, v]) => encode(k) + "=" + encode(v)).join("&"); }
    _changed() { if (this._url) this._url._search = this._list.length ? "?" + this.toString() : ""; }
  }

  const PARTS = /^([a-zA-Z][a-zA-Z0-9+.-]*:)(?:\/\/(?:([^:@\/?#]*)(?::([^@\/?#]*))?@)?([^:\/?#]*)(?::(\d*))?)?([^?#]*)(\?[^#]*)?(#.*)?$/;
  const DEFAULT_PORT = { "http:": "80", "https:": "443", "ws:": "80", "wss:": "443", "ftp:": "21" };
  function normalisePath(path) {
    const out = [];
    for (const seg of path.split("/")) {
      if (seg === "..") { if (out.length > 1) out.pop(); }
      else if (seg !== ".") out.push(seg);
    }
    let p = out.join("/");
    if (!p.startsWith("/")) p = "/" + p;
    if (/\/\.\.?$/.test(path)) p += p.endsWith("/") ? "" : "/";
    return p;
  }
  class URL {
    constructor(url, base) {
      url = String(url).trim();
      let m = PARTS.exec(url);
      if (!m) {
        if (base === undefined) throw new TypeError(`Failed to construct 'URL': Invalid URL '${url}'`);
        const b = new URL(base);
        if (url.startsWith("//")) m = PARTS.exec(b.protocol + url);
        else if (url.startsWith("/")) m = PARTS.exec(b.origin + url);
        else if (url.startsWith("?")) m = PARTS.exec(b.origin + b.pathname + url);
        else if (url.startsWith("#")) m = PARTS.exec(b.origin + b.pathname + b.search + url);
        else if (url === "") m = PARTS.exec(b.origin + b.pathname + b.search);
        else m = PARTS.exec(b.origin + b.pathname.replace(/[^/]*$/, "") + url);
        if (!m) throw new TypeError(`Failed to construct 'URL': Invalid URL '${url}'`);
      }
      this.protocol = m[1].toLowerCase();
      this.username = m[2] || "";
      this.password = m[3] || "";
      this.hostname = (m[4] || "").toLowerCase();
      this.port = m[5] && m[5] !== DEFAULT_PORT[this.protocol] ? m[5] : "";
      const hierarchical = url.slice(m[1].length).startsWith("//") || this.hostname;
      this.pathname = hierarchical ? normalisePath(m[6] || "/") : m[6] || "";
      this._search = m[7] && m[7] !== "?" ? m[7] : "";
      this.hash = m[8] && m[8] !== "#" ? m[8] : "";
      this._params = null;
    }
    get host() { return this.port ? this.hostname + ":" + this.port : this.hostname; }
    set host(v) { const [h, p] = String(v).split(":"); this.hostname = h; this.port = p || ""; }
    get origin() { return this.hostname ? this.protocol + "//" + this.host : "null"; }
    get search() { return this._search; }
    set search(v) { v = String(v); this._search = v && v !== "?" ? (v[0] === "?" ? v : "?" + v) : ""; this._params = null; }
    get searchParams() {
      if (!this._params) { this._params = new URLSearchParams(this._search); this._params._url = this; }
      return this._params;
    }
    get href() {
      const auth = this.username ? this.username + (this.password ? ":" + this.password : "") + "@" : "";
      const slashes = this.hostname || this.protocol === "file:" ? "//" : "";
      return this.protocol + slashes + auth + this.host + this.pathname + this._search + this.hash;
    }
    set href(v) { Object.assign(this, new URL(v)); }
    toString() { return this.href; }
    toJSON() { return this.href; }
    static canParse(url, base) { try { new URL(url, base); return true; } catch { return false; } }
  }
  define("URL", URL);
  define("URLSearchParams", URLSearchParams);

  // ---------------------------------------------------------------- network
  const request = globalThis.__nebula_request;

  class Headers {
    constructor(init) {
      this._map = new Map();
      if (init instanceof Headers) init.forEach((v, k) => this.append(k, v));
      else if (Array.isArray(init)) for (const [k, v] of init) this.append(k, v);
      else if (init && typeof init === "object") for (const k of Object.keys(init)) this.append(k, init[k]);
    }
    append(k, v) {
      k = String(k).toLowerCase();
      const cur = this._map.get(k);
      this._map.set(k, cur === undefined ? String(v) : cur + ", " + v);
    }
    set(k, v) { this._map.set(String(k).toLowerCase(), String(v)); }
    get(k) { const v = this._map.get(String(k).toLowerCase()); return v === undefined ? null : v; }
    has(k) { return this._map.has(String(k).toLowerCase()); }
    delete(k) { this._map.delete(String(k).toLowerCase()); }
    forEach(f, self) { for (const [k, v] of this.entries()) f.call(self, v, k, this); }
    keys() { return [...this._map.keys()].sort()[Symbol.iterator](); }
    values() { return [...this.entries()].map(e => e[1])[Symbol.iterator](); }
    entries() { return [...this._map.keys()].sort().map(k => [k, this._map.get(k)])[Symbol.iterator](); }
    [Symbol.iterator]() { return this.entries(); }
  }

  class Response {
    constructor(body = null, init = {}) {
      this._body = body === null || body === undefined ? "" : String(body);
      this.status = init.status ?? 200;
      this.statusText = init.statusText ?? "";
      this.headers = new Headers(init.headers);
      this.url = init.url ?? "";
      this.redirected = false;
      this.type = "basic";
      this.bodyUsed = false;
    }
    get ok() { return this.status >= 200 && this.status < 300; }
    _take() {
      if (this.bodyUsed) return Promise.reject(new TypeError("Body has already been consumed."));
      this.bodyUsed = true;
      return Promise.resolve(this._body);
    }
    text() { return this._take(); }
    json() { return this._take().then(t => JSON.parse(t)); }
    clone() { return new Response(this._body, { status: this.status, statusText: this.statusText, headers: this.headers, url: this.url }); }
    static json(data, init = {}) {
      return new Response(JSON.stringify(data), { ...init, headers: { "content-type": "application/json", ...(init.headers || {}) } });
    }
    static error() { return new Response(null, { status: 0 }); }
  }

  class Request {
    constructor(input, init = {}) {
      const base = input instanceof Request ? input : null;
      this.url = base ? base.url : String(input);
      this.method = String(init.method || (base && base.method) || "GET").toUpperCase();
      this.headers = new Headers(init.headers || (base && base.headers));
      this.body = init.body ?? (base ? base.body : null);
      this.signal = init.signal || (base && base.signal) || null;
      this.credentials = init.credentials || "same-origin";
      this.mode = init.mode || "cors";
    }
    clone() { return new Request(this); }
  }

  class AbortSignal {
    constructor() { this.aborted = false; this.reason = undefined; this._listeners = []; this.onabort = null; }
    addEventListener(type, f) { if (type === "abort") this._listeners.push(f); }
    removeEventListener(type, f) { this._listeners = this._listeners.filter(g => g !== f); }
    throwIfAborted() { if (this.aborted) throw this.reason; }
    _abort(reason) {
      if (this.aborted) return;
      this.aborted = true;
      this.reason = reason;
      const ev = { type: "abort", target: this };
      if (this.onabort) this.onabort(ev);
      for (const f of this._listeners) f(ev);
    }
    static abort(reason) { const s = new AbortSignal(); s._abort(reason ?? abortError()); return s; }
    static timeout(ms) { const s = new AbortSignal(); setTimeout(() => s._abort(abortError("TimeoutError")), ms); return s; }
  }
  function abortError(name = "AbortError") {
    const e = new Error(name === "AbortError" ? "The operation was aborted." : "The operation timed out.");
    e.name = name;
    return e;
  }
  class AbortController {
    constructor() { this.signal = new AbortSignal(); }
    abort(reason) { this.signal._abort(reason ?? abortError()); }
  }

  class FormData {
    constructor(form) {
      this._list = [];
      if (form && form.querySelectorAll) {
        for (const el of form.querySelectorAll("input, select, textarea")) {
          if (!el.name || el.disabled) continue;
          if ((el.type === "checkbox" || el.type === "radio") && !el.checked) continue;
          if (["submit", "button", "reset", "file"].includes(el.type)) continue;
          this._list.push([el.name, el.value]);
        }
      }
    }
    append(k, v) { this._list.push([String(k), String(v)]); }
    set(k, v) { this.delete(k); this.append(k, v); }
    get(k) { const e = this._list.find(e => e[0] === k); return e ? e[1] : null; }
    getAll(k) { return this._list.filter(e => e[0] === k).map(e => e[1]); }
    has(k) { return this._list.some(e => e[0] === k); }
    delete(k) { this._list = this._list.filter(e => e[0] !== k); }
    entries() { return this._list.map(e => [e[0], e[1]])[Symbol.iterator](); }
    keys() { return this._list.map(e => e[0])[Symbol.iterator](); }
    values() { return this._list.map(e => e[1])[Symbol.iterator](); }
    forEach(f, self) { for (const [k, v] of this._list) f.call(self, v, k, this); }
    [Symbol.iterator]() { return this.entries(); }
  }

  /// The request body as text, plus a content type to send with it.
  function encodeBody(body) {
    if (body === null || body === undefined) return [null, null];
    if (body instanceof URLSearchParams) return [body.toString(), "application/x-www-form-urlencoded;charset=UTF-8"];
    if (body instanceof FormData) {
      return [new URLSearchParams([...body]).toString(), "application/x-www-form-urlencoded;charset=UTF-8"];
    }
    return [String(body), "text/plain;charset=UTF-8"];
  }

  function fetch(input, init = {}) {
    let req;
    try { req = new Request(input, init); } catch (e) { return Promise.reject(e); }
    if (req.signal && req.signal.aborted) return Promise.reject(req.signal.reason);
    const [body, type] = encodeBody(req.body);
    if (type && !req.headers.has("content-type")) req.headers.set("content-type", type);
    const pending = request(req.method, req.url, [...req.headers], body).then(raw =>
      new Response(raw.body, { status: raw.status, statusText: raw.statusText, headers: raw.headers, url: raw.url }));
    if (!req.signal) return pending;
    return new Promise((resolve, reject) => {
      req.signal.addEventListener("abort", () => reject(req.signal.reason));
      pending.then(resolve, reject);
    });
  }

  class XMLHttpRequest {
    constructor() {
      this.readyState = 0;
      this.status = 0;
      this.statusText = "";
      this.responseText = "";
      this.response = "";
      this.responseType = "";
      this.responseURL = "";
      this.timeout = 0;
      this.withCredentials = false;
      this._headers = [];
      this._responseHeaders = new Headers();
      this._listeners = {};
      this.upload = { addEventListener() {}, removeEventListener() {} };
    }
    open(method, url) {
      this._method = String(method).toUpperCase();
      this._url = String(url);
      this._aborted = false;
      this._setState(1);
    }
    setRequestHeader(k, v) { this._headers.push([String(k), String(v)]); }
    getResponseHeader(k) { return this._responseHeaders.get(k); }
    getAllResponseHeaders() { return [...this._responseHeaders].map(([k, v]) => k + ": " + v + "\r\n").join(""); }
    overrideMimeType() {}
    addEventListener(type, f) { (this._listeners[type] ||= []).push(f); }
    removeEventListener(type, f) { this._listeners[type] = (this._listeners[type] || []).filter(g => g !== f); }
    _fire(type) {
      const ev = { type, target: this, currentTarget: this, loaded: this.responseText.length, total: this.responseText.length };
      const h = this["on" + type];
      if (typeof h === "function") h.call(this, ev);
      for (const f of this._listeners[type] || []) f.call(this, ev);
    }
    _setState(n) { this.readyState = n; this._fire("readystatechange"); }
    send(body) {
      const [text, type] = encodeBody(this._method === "GET" || this._method === "HEAD" ? null : body);
      if (type && !this._headers.some(([k]) => k.toLowerCase() === "content-type")) this._headers.push(["content-type", type]);
      this._fire("loadstart");
      request(this._method, this._url, this._headers, text).then(raw => {
        if (this._aborted) return;
        this.status = raw.status;
        this.statusText = raw.statusText;
        this.responseURL = raw.url;
        this._responseHeaders = new Headers(raw.headers);
        this._setState(2);
        this._setState(3);
        this.responseText = raw.body;
        if (this.responseType === "json") {
          try { this.response = JSON.parse(raw.body); } catch { this.response = null; }
        } else {
          this.response = raw.body;
        }
        this._setState(4);
        this._fire("load");
        this._fire("loadend");
      }, () => {
        if (this._aborted) return;
        this._setState(4);
        this._fire("error");
        this._fire("loadend");
      });
    }
    abort() { this._aborted = true; this.readyState = 0; this._fire("abort"); }
  }
  XMLHttpRequest.UNSENT = 0; XMLHttpRequest.OPENED = 1; XMLHttpRequest.HEADERS_RECEIVED = 2;
  XMLHttpRequest.LOADING = 3; XMLHttpRequest.DONE = 4;

  for (const [name, value] of Object.entries({ Headers, Response, Request, AbortController, AbortSignal, FormData, XMLHttpRequest, fetch })) {
    define(name, value);
  }

  // ---------------------------------------------------------------- text and crypto
  class TextEncoder {
    get encoding() { return "utf-8"; }
    encode(input = "") {
      const s = String(input);
      const out = [];
      for (let i = 0; i < s.length; i++) {
        let c = s.charCodeAt(i);
        if (c >= 0xd800 && c < 0xdc00 && i + 1 < s.length) {
          const d = s.charCodeAt(i + 1);
          if (d >= 0xdc00 && d < 0xe000) { c = 0x10000 + ((c - 0xd800) << 10) + (d - 0xdc00); i++; }
        }
        if (c >= 0xd800 && c < 0xe000) c = 0xfffd;
        if (c < 0x80) out.push(c);
        else if (c < 0x800) out.push(0xc0 | (c >> 6), 0x80 | (c & 63));
        else if (c < 0x10000) out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
        else out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 63), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
      }
      return new Uint8Array(out);
    }
    encodeInto(s, dest) {
      const bytes = this.encode(s);
      const n = Math.min(bytes.length, dest.length);
      dest.set(bytes.subarray(0, n));
      return { read: s.length, written: n };
    }
  }
  class TextDecoder {
    constructor(label = "utf-8", options = {}) {
      const l = String(label).toLowerCase();
      if (!["utf-8", "utf8", "unicode-1-1-utf-8"].includes(l)) throw new RangeError(`The encoding '${label}' is not supported`);
      this.fatal = !!options.fatal;
      this.ignoreBOM = !!options.ignoreBOM;
    }
    get encoding() { return "utf-8"; }
    decode(input) {
      if (input === undefined) return "";
      const b = input instanceof ArrayBuffer ? new Uint8Array(input)
        : ArrayBuffer.isView(input) ? new Uint8Array(input.buffer, input.byteOffset, input.byteLength) : null;
      if (!b) throw new TypeError("TextDecoder.decode: the input is not a buffer");
      let s = "", i = 0;
      if (!this.ignoreBOM && b[0] === 0xef && b[1] === 0xbb && b[2] === 0xbf) i = 3;
      const bad = () => { if (this.fatal) throw new TypeError("The encoded data was not valid."); return "�"; };
      while (i < b.length) {
        const c = b[i];
        let n = c < 0x80 ? 0 : c >= 0xf0 && c < 0xf5 ? 3 : c >= 0xe0 ? 2 : c >= 0xc2 && c < 0xe0 ? 1 : -1;
        if (n < 0 || c >= 0xf5) { s += bad(); i++; continue; }
        let cp = n === 0 ? c : c & (0x3f >> n);
        let ok = true;
        for (let k = 1; k <= n; k++) {
          const d = b[i + k];
          if (d === undefined || (d & 0xc0) !== 0x80) { ok = false; break; }
          cp = (cp << 6) | (d & 63);
        }
        if (!ok || (n === 2 && (cp < 0x800 || (cp >= 0xd800 && cp < 0xe000))) || (n === 3 && (cp < 0x10000 || cp > 0x10ffff))) {
          s += bad(); i++; continue;
        }
        s += String.fromCodePoint(cp);
        i += n + 1;
      }
      return s;
    }
  }
  const crypto = {
    getRandomValues(array) { return __nebula_random(array); },
    randomUUID() {
      const b = __nebula_random(new Uint8Array(16));
      b[6] = (b[6] & 0x0f) | 0x40;
      b[8] = (b[8] & 0x3f) | 0x80;
      const h = [...b].map(x => x.toString(16).padStart(2, "0")).join("");
      return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
    },
  };
  for (const [name, value] of Object.entries({ TextEncoder, TextDecoder, crypto })) define(name, value);
})(globalThis);
