// Web APIs written in JavaScript, run in every page's realm before its
// scripts. Observers report once (asynchronously) and never again: enough
// for lazy loading and feature checks, not live tracking.
(function (global) {
  "use strict";
  const define = (name, value) =>
    Object.defineProperty(global, name, { value, writable: true, configurable: true, enumerable: false });
  const later = f => setTimeout(f, 0);

  class MutationObserver {
    constructor(callback) { this._callback = callback; }
    observe() {}
    disconnect() {}
    takeRecords() { return []; }
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
