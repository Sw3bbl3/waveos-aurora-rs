// Web APIs written in JavaScript, run in every page's realm before its
// scripts. MutationObserver is live; the intersection and resize observers
// report once (asynchronously) and never again: enough for lazy loading and
// feature checks, not live tracking.
(function (global) {
  "use strict";
  const define = (name, value) =>
    Object.defineProperty(global, name, { value, writable: true, configurable: true, enumerable: false });
  const later = f => setTimeout(f, 0);

  // EventTarget is constructible, and objects that aren't nodes (XHR,
  // FileReader, MediaSource, libraries' own emitters) keep their
  // listeners here. Nodes and the window use the browser's dispatch.
  const NativeTarget = global.EventTarget, targetProto = NativeTarget.prototype;
  const nativeAdd = targetProto.addEventListener, nativeRemove = targetProto.removeEventListener;
  const nativeDispatch = targetProto.dispatchEvent;
  const jsListeners = new WeakMap(), stoppedNow = new WeakSet();
  // A bare addEventListener(…) call targets the window.
  const isNative = t => t == null || t === global || t instanceof Node;
  const captureOf = o => typeof o === "object" && o !== null ? !!o.capture : !!o;
  function EventTarget() {
    if (!new.target) throw new TypeError("Failed to construct 'EventTarget': Please use the 'new' operator");
  }
  EventTarget.prototype = targetProto;
  Object.defineProperty(targetProto, "constructor", { value: EventTarget, writable: true, configurable: true });
  Object.defineProperties(targetProto, {
    addEventListener: { configurable: true, writable: true, value: function addEventListener(type, callback, options) {
      if (isNative(this)) return nativeAdd.call(this == null ? global : this, type, callback, options);
      if (callback == null) return;
      let map = jsListeners.get(this);
      if (!map) jsListeners.set(this, map = new Map());
      type = String(type);
      const list = map.get(type) || [], capture = captureOf(options);
      if (list.some(l => l.callback === callback && l.capture === capture)) return;
      const once = typeof options === "object" && options !== null && !!options.once;
      const signal = typeof options === "object" && options !== null ? options.signal : undefined;
      if (signal && signal.aborted) return;
      const entry = { callback, capture, once, removed: false };
      list.push(entry); map.set(type, list);
      if (signal) signal.addEventListener("abort", () => this.removeEventListener(type, callback, { capture }));
    }},
    removeEventListener: { configurable: true, writable: true, value: function removeEventListener(type, callback, options) {
      if (isNative(this)) return nativeRemove.call(this == null ? global : this, type, callback, options);
      const map = jsListeners.get(this), list = map && map.get(String(type));
      if (!list) return;
      const capture = captureOf(options);
      const i = list.findIndex(l => l.callback === callback && l.capture === capture);
      if (i >= 0) { list[i].removed = true; list.splice(i, 1); }
    }},
    dispatchEvent: { configurable: true, writable: true, value: function dispatchEvent(event) {
      if (isNative(this)) return nativeDispatch.call(this == null ? global : this, event);
      if (!(event instanceof Event)) throw new TypeError("Failed to execute 'dispatchEvent' on 'EventTarget': parameter 1 is not of type 'Event'.");
      const set = (k, v) => Object.defineProperty(event, k, { value: v, configurable: true, enumerable: true, writable: true });
      set("target", this); set("currentTarget", this); set("eventPhase", 2);
      if (!("srcElement" in event)) set("srcElement", this);
      const map = jsListeners.get(this), list = map && map.get(event.type);
      for (const l of list ? list.slice() : []) {
        if (l.removed) continue;
        if (l.once) this.removeEventListener(event.type, l.callback, { capture: l.capture });
        try {
          if (typeof l.callback === "function") l.callback.call(this, event);
          else if (l.callback && typeof l.callback.handleEvent === "function") l.callback.handleEvent(event);
        } catch (e) { console.error("Uncaught " + (e && e.stack || e)); }
        if (stoppedNow.has(event)) break;
      }
      set("currentTarget", null); set("eventPhase", 0);
      return !event.defaultPrevented;
    }},
  });
  const nativeStopNow = Event.prototype.stopImmediatePropagation;
  Object.defineProperty(Event.prototype, "stopImmediatePropagation", { configurable: true, writable: true,
    value: function stopImmediatePropagation() { stoppedNow.add(this); return nativeStopNow.call(this); } });
  define("EventTarget", EventTarget);

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

  class WheelEvent extends MouseEvent {
    constructor(type, init = {}) {
      super(type, init);
      for (const key of ["deltaX", "deltaY", "deltaZ", "clientX", "clientY", "screenX", "screenY"])
        Object.defineProperty(this, key, {value: Number(init[key] || 0), enumerable: true});
      Object.defineProperty(this, "deltaMode", {value: Number(init.deltaMode || 0) >>> 0, enumerable: true});
    }
  }
  for (const [name, value] of Object.entries({DOM_DELTA_PIXEL:0, DOM_DELTA_LINE:1, DOM_DELTA_PAGE:2})) {
    Object.defineProperty(WheelEvent, name, {value}); Object.defineProperty(WheelEvent.prototype, name, {value});
  }
  define("WheelEvent", WheelEvent);

  // Same-realm message channels: messages are cloned at send time and
  // delivered as separate tasks, after start() or assignment to onmessage.
  const portStates = new WeakMap();
  const portToken = {};
  const portState = p => {
    const s = portStates.get(p);
    if (!s) throw new TypeError("Illegal MessagePort invocation");
    return s;
  };
  function cloneMessage(value, seen = new Map()) {
    if (typeof value === "function" || typeof value === "symbol")
      throw new DOMException("Value cannot be cloned", "DataCloneError");
    if (value === null || typeof value !== "object") return value;
    if (seen.has(value)) return seen.get(value);
    if (value instanceof Node || portStates.has(value))
      throw new DOMException("Object cannot be cloned", "DataCloneError");
    let copy;
    if (value instanceof Date) copy = new Date(value.getTime());
    else if (value instanceof RegExp) copy = new RegExp(value.source, value.flags);
    else if (value instanceof ArrayBuffer) copy = value.slice(0);
    else if (ArrayBuffer.isView(value)) {
      const buffer = cloneMessage(value.buffer, seen);
      copy = value instanceof DataView ? new DataView(buffer, value.byteOffset, value.byteLength)
        : new value.constructor(buffer, value.byteOffset, value.length);
    } else if (value instanceof Map) {
      copy = new Map(); seen.set(value, copy);
      value.forEach((v, k) => copy.set(cloneMessage(k, seen), cloneMessage(v, seen)));
    } else if (value instanceof Set) {
      copy = new Set(); seen.set(value, copy);
      value.forEach(v => copy.add(cloneMessage(v, seen)));
    } else {
      const proto = Object.getPrototypeOf(value);
      if (!Array.isArray(value) && proto !== Object.prototype && proto !== null)
        throw new DOMException("Unsupported clone type", "DataCloneError");
      copy = Array.isArray(value) ? new Array(value.length) : {};
      seen.set(value, copy);
      for (const key of Object.keys(value)) Object.defineProperty(copy, key, {
        value: cloneMessage(value[key], seen), writable: true, enumerable: true, configurable: true
      });
    }
    seen.set(value, copy);
    return copy;
  }
  function schedulePort(port) {
    const s = portState(port);
    if (!s.started || s.closed || s.scheduled || !s.queue.length) return;
    s.scheduled = true;
    later(() => {
      s.scheduled = false;
      if (!s.closed && s.started && s.queue.length) {
        const event = new Event("message");
        Object.defineProperties(event, {
          data: { value: s.queue.shift() }, target: { value: port }, currentTarget: { value: port },
          origin: { value: "" }, ports: { value: [] }, source: { value: null }
        });
        port.dispatchEvent(event);
      }
      schedulePort(port);
    });
  }
  class MessagePort {
    constructor(token) {
      if (token !== portToken) throw new TypeError("Illegal constructor");
      portStates.set(this, { peer: null, started: false, closed: false, scheduled: false,
        queue: [], listeners: [], handler: null });
    }
    get onmessage() { return portState(this).handler; }
    set onmessage(value) {
      portState(this).handler = typeof value === "function" ? value : null;
      this.start();
    }
    start() { portState(this).started = true; schedulePort(this); }
    close() {
      const s = portState(this); s.closed = true; s.queue = [];
      if (s.peer) portState(s.peer).peer = null;
      s.peer = null;
    }
    postMessage(value, options) {
      const s = portState(this);
      const transfer = Array.isArray(options) ? options : options && options.transfer;
      if (transfer && transfer.length)
        throw new DOMException("Transfer lists are not yet supported", "DataCloneError");
      const copy = cloneMessage(value);
      if (s.closed || !s.peer) return;
      portState(s.peer).queue.push(copy); schedulePort(s.peer);
    }
    addEventListener(type, callback, options) {
      const s = portState(this);
      if (String(type) !== "message" || callback == null) return;
      if (!s.listeners.some(l => l.callback === callback))
        s.listeners.push({ callback, once: !!(options && options.once) });
    }
    removeEventListener(type, callback) {
      const s = portState(this);
      if (String(type) === "message") s.listeners = s.listeners.filter(l => l.callback !== callback);
    }
    dispatchEvent(event) {
      const s = portState(this);
      if (event.type !== "message") return true;
      const listeners = s.listeners.slice();
      if (s.handler) listeners.push({ callback: s.handler, once: false });
      for (const l of listeners) {
        if (l.once) this.removeEventListener("message", l.callback);
        try {
          if (typeof l.callback === "function") l.callback.call(this, event);
          else l.callback.handleEvent(event);
        } catch (error) { console.error(error); }
      }
      return !event.defaultPrevented;
    }
  }
  Object.setPrototypeOf(MessagePort.prototype, EventTarget.prototype);
  class MessageChannel {
    constructor() {
      const port1 = new MessagePort(portToken), port2 = new MessagePort(portToken);
      portState(port1).peer = port2; portState(port2).peer = port1;
      Object.defineProperties(this, { port1: { value: port1, enumerable: true },
        port2: { value: port2, enumerable: true } });
    }
  }
  define("MessagePort", MessagePort);
  define("MessageChannel", MessageChannel);

  const ranges = new WeakMap();
  const rangeState = r => {
    const s = ranges.get(r); if (!s) throw new TypeError("Illegal Range invocation"); return s;
  };
  const nodeLength = n => n instanceof CharacterData ? n.length : n.childNodes.length;
  const nodeIndex = n => Array.prototype.indexOf.call(n.parentNode.childNodes, n);
  const ancestors = n => { const a = []; for (; n; n = n.parentNode) a.unshift(n); return a; };
  const comparePoints = (a, ao, b, bo) => {
    if (a === b) return Math.sign(ao - bo);
    const aa = ancestors(a), bb = ancestors(b);
    if (aa[0] !== bb[0]) throw new DOMException("Different roots", "WrongDocumentError");
    let i = 0; while (i < aa.length && i < bb.length && aa[i] === bb[i]) i++;
    if (i === aa.length) return ao <= nodeIndex(bb[i]) ? -1 : 1;
    if (i === bb.length) return nodeIndex(aa[i]) < bo ? -1 : 1;
    return Math.sign(nodeIndex(aa[i]) - nodeIndex(bb[i]));
  };
  function rangeSet(r, end, node, offset) {
    if (!(node instanceof Node)) throw new TypeError("Range boundary requires a Node");
    offset = Number(offset) >>> 0;
    if (offset > nodeLength(node)) throw new DOMException("Offset exceeds node", "IndexSizeError");
    const s = rangeState(r), other = end ? "start" : "end", key = end ? "end" : "start";
    const separate = ancestors(node)[0] !== ancestors(s[other])[0];
    if (separate || (end ? comparePoints(node, offset, s.start, s.startOffset) < 0
                        : comparePoints(node, offset, s.end, s.endOffset) > 0)) {
      s[other] = node; s[other + "Offset"] = offset;
    }
    s[key] = node; s[key + "Offset"] = offset;
  }
  function rangeContents(r, remove) {
    const s = rangeState(r), result = document.createDocumentFragment();
    if (r.collapsed) return result;
    function plan(n) {
      if (n instanceof CharacterData) return {node:n,
        a:n === s.start ? s.startOffset : 0, b:n === s.end ? s.endOffset : n.length};
      const children = [];
      for (const child of Array.from(n.childNodes)) {
        const i = nodeIndex(child);
        if (comparePoints(n, i+1, s.start, s.startOffset) <= 0 || comparePoints(n, i, s.end, s.endOffset) >= 0) continue;
        const full = comparePoints(n, i, s.start, s.startOffset) >= 0 && comparePoints(n, i+1, s.end, s.endOffset) <= 0;
        children.push(full ? {node:child, full:true} : plan(child));
      }
      return {node:n, children};
    }
    function apply(p, target, root) {
      if (p.full) { target.appendChild(remove ? p.node : p.node.cloneNode(true)); return; }
      if (p.node instanceof CharacterData) {
        const clone = p.node.cloneNode(false); clone.data = p.node.data.slice(p.a, p.b); target.appendChild(clone);
        if (remove) p.node.deleteData(p.a, p.b-p.a);
        return;
      }
      if (!root) { const clone = p.node.cloneNode(false); target.appendChild(clone); target = clone; }
      for (const child of p.children) apply(child, target, false);
    }
    apply(plan(r.commonAncestorContainer), result, true);
    if (remove) r.collapse(true);
    return result;
  }
  class Range {
    constructor() { ranges.set(this, {start: document, end: document, startOffset: 0, endOffset: 0}); }
    get startContainer() { return rangeState(this).start; }
    get endContainer() { return rangeState(this).end; }
    get startOffset() { return rangeState(this).startOffset; }
    get endOffset() { return rangeState(this).endOffset; }
    get collapsed() { const s = rangeState(this); return s.start === s.end && s.startOffset === s.endOffset; }
    get commonAncestorContainer() {
      const s = rangeState(this), a = ancestors(s.start), b = ancestors(s.end); let i = 0;
      while (i < a.length && i < b.length && a[i] === b[i]) i++;
      return a[i-1];
    }
    setStart(n, o) { rangeSet(this, false, n, o); }
    setEnd(n, o) { rangeSet(this, true, n, o); }
    setStartBefore(n) { this.setStart(n.parentNode, nodeIndex(n)); }
    setStartAfter(n) { this.setStart(n.parentNode, nodeIndex(n)+1); }
    setEndBefore(n) { this.setEnd(n.parentNode, nodeIndex(n)); }
    setEndAfter(n) { this.setEnd(n.parentNode, nodeIndex(n)+1); }
    selectNode(n) { this.setStartBefore(n); this.setEndAfter(n); }
    selectNodeContents(n) { this.setStart(n, 0); this.setEnd(n, nodeLength(n)); }
    collapse(start = false) {
      const s = rangeState(this);
      if (start) { s.end = s.start; s.endOffset = s.startOffset; }
      else { s.start = s.end; s.startOffset = s.endOffset; }
    }
    cloneRange() { const r = new Range(); ranges.set(r, Object.assign({}, rangeState(this))); return r; }
    cloneContents() { return rangeContents(this, false); }
    extractContents() { return rangeContents(this, true); }
    deleteContents() { this.extractContents(); }
    createContextualFragment(html) {
      rangeState(this); const t = document.createElement("template"); t.innerHTML = String(html); return t.content;
    }
    insertNode(node) {
      const s = rangeState(this); let parent = s.start, before = parent.childNodes[s.startOffset] || null;
      if (parent instanceof Text) { before = parent.splitText(s.startOffset); parent = parent.parentNode; }
      if (!parent) throw new DOMException("Detached boundary", "HierarchyRequestError");
      parent.insertBefore(node, before);
    }
    toString() { return this.cloneContents().textContent; }
    detach() { rangeState(this); }
  }
  for (const [name, value] of Object.entries({START_TO_START:0, START_TO_END:1, END_TO_END:2, END_TO_START:3})) {
    Object.defineProperty(Range, name, {value}); Object.defineProperty(Range.prototype, name, {value});
  }
  define("Range", Range);
  Object.defineProperty(Document.prototype, "createRange", {configurable:true, writable:true, value() { return new Range(); }});

  const pendingPlays = new WeakMap();
  function settlePlays(el, error) {
    const list = pendingPlays.get(el);
    if (!list || !list.length) return;
    for (const p of list.splice(0)) error ? p.reject(error) : p.resolve();
  }
  for (const name of ["paused", "ended", "duration", "readyState", "currentSrc", "seeking"])
    Object.defineProperty(HTMLMediaElement.prototype, name, { configurable: true,
      get() { return this._mediaState()[name]; } });
  for (const name of ["videoWidth", "videoHeight"])
    Object.defineProperty(HTMLVideoElement.prototype, name, { configurable: true,
      get() { return this._mediaState()[name]; } });
  for (const name of ["controls", "autoplay", "loop"])
    Object.defineProperty(HTMLMediaElement.prototype, name, { configurable: true,
      get() { return this.hasAttribute(name); },
      set(value) { this.toggleAttribute(name, !!value); } });
  Object.defineProperties(HTMLMediaElement.prototype, {
    muted: { configurable: true, get() { return this._mediaState().muted; },
      set(value) { this._mediaCommand("muted", value ? 1 : 0); this.dispatchEvent(new Event("volumechange")); } },
    defaultMuted: { configurable: true, get() { return this.hasAttribute("muted"); },
      set(value) { this.toggleAttribute("muted", !!value); } },
    volume: { configurable: true, get() { return this._mediaState().volume; },
      set(value) {
        value = Number(value);
        if (!Number.isFinite(value)) throw new TypeError("Invalid volume");
        if (value < 0 || value > 1) throw new DOMException("Volume must be between zero and one", "IndexSizeError");
        this._mediaCommand("volume", value); this.dispatchEvent(new Event("volumechange"));
      } },
    currentTime: { configurable: true, get() { return this._mediaState().currentTime; },
      set(value) { this._mediaCommand("seek", Number(value)); } },
    error: { configurable: true, get() {
      const error = this._mediaState().error;
      return error ? {code: 4, message: error} : null;
    } },
    play: { configurable: true, writable: true, value() {
      const s = this._mediaState();
      if (s.error) return Promise.reject(new DOMException(s.error, "NotSupportedError"));
      const wasPaused = s.paused;
      const promise = new Promise((resolve, reject) => {
        let list = pendingPlays.get(this);
        if (!list) {
          pendingPlays.set(this, list = []);
          // Pending play() promises settle on these (spec: "notify about playing").
          this.addEventListener("playing", () => settlePlays(this, null));
          this.addEventListener("error", () => settlePlays(this, new DOMException(this._mediaState().error || "Failed to load because no supported source was found.", "NotSupportedError")));
        }
        list.push({ resolve, reject });
      });
      this._mediaCommand("play");
      if (wasPaused) queueMicrotask(() => this.dispatchEvent(new Event("play")));
      if (!wasPaused && s.readyState >= 3) queueMicrotask(() => settlePlays(this, null));
      return promise;
    } },
    pause: { configurable: true, writable: true, value() {
      const wasPaused = this.paused; this._mediaCommand("pause");
      settlePlays(this, new DOMException("The play() request was interrupted by a call to pause().", "AbortError"));
      if (!wasPaused) queueMicrotask(() => { this.dispatchEvent(new Event("timeupdate")); this.dispatchEvent(new Event("pause")); });
    } },
    load: { configurable: true, writable: true, value() {
      settlePlays(this, new DOMException("The play() request was interrupted by a new load request.", "AbortError"));
      this._mediaCommand("load"); queueMicrotask(() => this.dispatchEvent(new Event("emptied")));
    } },
    canPlayType: { configurable: true, writable: true, value(type) {
      // Audio and MediaSource codecs must not be advertised before supported.
      type = String(type).toLowerCase();
      if (!/^video\/mp4(?:\s*;|$)/.test(type)) return "";
      if (type.indexOf("codecs") < 0) return "maybe";
      return /codecs\s*=\s*["']?avc[13]\.[0-9a-f]{6}["']?\s*$/.test(type) ? "probably" : "";
    } }
  });
  for (const [name, value] of Object.entries({HAVE_NOTHING: 0, HAVE_METADATA: 1,
    HAVE_CURRENT_DATA: 2, HAVE_FUTURE_DATA: 3, HAVE_ENOUGH_DATA: 4})) {
    Object.defineProperty(HTMLMediaElement, name, {value});
    Object.defineProperty(HTMLMediaElement.prototype, name, {value});
  }

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

  // ImageData: pixels as RGBA bytes, for canvas getImageData/putImageData.
  class ImageData {
    constructor(a, b, c) {
      let data, width, height;
      if (a instanceof Uint8ClampedArray) {
        data = a; width = b >>> 0;
        if (!width || data.length % 4 || (data.length / 4) % width) {
          throw new DOMException("The input data has an invalid length", "IndexSizeError");
        }
        height = c === undefined ? data.length / 4 / width : c >>> 0;
        if (height * width * 4 !== data.length) throw new DOMException("The input data has an invalid length", "IndexSizeError");
      } else {
        width = a >>> 0; height = b >>> 0;
        if (!width || !height) throw new DOMException("The source width or height is 0", "IndexSizeError");
        if (width * height > 1024 * 1024) throw new RangeError("ImageData exceeds the pixel budget");
        data = new Uint8ClampedArray(width * height * 4);
      }
      Object.defineProperties(this, {
        data: { value: data, enumerable: true }, width: { value: width, enumerable: true },
        height: { value: height, enumerable: true }, colorSpace: { value: "srgb", enumerable: true },
      });
    }
  }
  define("ImageData", ImageData);

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

  // Bytes: Blob, File, FileReader and a ReadableStream of chunks.
  const decodeText = (bytes, type) => __nebula_text(bytes, type || "");
  const utf8 = s => new TextEncoder().encode(String(s));
  function bytesOf(part) {
    if (part instanceof Blob) return blobBytes(part);
    if (part instanceof ArrayBuffer) return new Uint8Array(part.slice(0));
    if (ArrayBuffer.isView(part)) return new Uint8Array(part.buffer.slice(part.byteOffset, part.byteOffset + part.byteLength));
    return utf8(part);
  }
  function concat(chunks) {
    let n = 0; for (const c of chunks) n += c.length;
    const out = new Uint8Array(n); let at = 0;
    for (const c of chunks) { out.set(c, at); at += c.length; }
    return out;
  }
  const blobData = new WeakMap();
  const blobBytes = b => { const d = blobData.get(b); if (!d) throw new TypeError("Illegal Blob invocation"); return d.bytes; };
  class Blob {
    constructor(parts = [], options = {}) {
      if (parts === null || typeof parts !== "object" || typeof parts[Symbol.iterator] !== "function")
        throw new TypeError("Failed to construct 'Blob': The provided value cannot be converted to a sequence.");
      const type = options && options.type !== undefined ? String(options.type) : "";
      blobData.set(this, { bytes: concat(Array.from(parts, bytesOf)), type: /^[\x20-\x7e]*$/.test(type) ? type.toLowerCase() : "" });
    }
    get size() { return blobBytes(this).length; }
    get type() { blobBytes(this); return blobData.get(this).type; }
    slice(start = 0, end, type = "") {
      const b = blobBytes(this), n = b.length;
      const rel = (v, d) => v === undefined ? d : v < 0 ? Math.max(n + Math.trunc(v), 0) : Math.min(Math.trunc(v), n);
      const s = rel(start, 0), e = rel(end, n);
      const out = new Blob([], { type }); blobData.get(out).bytes = b.slice(s, Math.max(s, e));
      return out;
    }
    arrayBuffer() { try { return Promise.resolve(blobBytes(this).slice().buffer); } catch (e) { return Promise.reject(e); } }
    bytes() { try { return Promise.resolve(blobBytes(this).slice()); } catch (e) { return Promise.reject(e); } }
    text() { try { return Promise.resolve(decodeText(blobBytes(this))); } catch (e) { return Promise.reject(e); } }
    stream() { const b = blobBytes(this); return new ReadableStream({ start(c) { if (b.length) c.enqueue(b.slice()); c.close(); } }); }
    get [Symbol.toStringTag]() { return "Blob"; }
  }
  class File extends Blob {
    constructor(parts, name, options = {}) {
      super(parts, options);
      if (arguments.length < 2) throw new TypeError("Failed to construct 'File': 2 arguments required.");
      Object.defineProperty(this, "_file", { value: { name: String(name), lastModified: options.lastModified ?? Date.now() } });
    }
    get name() { return this._file.name; }
    get lastModified() { return this._file.lastModified; }
    get webkitRelativePath() { return ""; }
    get [Symbol.toStringTag]() { return "File"; }
  }
  class FileReader extends EventTarget {
    constructor() { super(); this.readyState = 0; this.result = null; this.error = null; }
    _read(blob, how) {
      if (!(blob instanceof Blob)) throw new TypeError("Failed to execute on 'FileReader': parameter 1 is not of type 'Blob'.");
      if (this.readyState === 1) throw new DOMException("The object is already busy reading Blobs.", "InvalidStateError");
      this.readyState = 1; this.result = null;
      const bytes = blobBytes(blob);
      later(() => {
        if (this.readyState !== 1) return;
        this._fire("loadstart");
        if (how === "buffer") this.result = bytes.slice().buffer;
        else if (how === "binary") { let s = ""; for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]); this.result = s; }
        else if (how === "url") {
          let s = ""; for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
          this.result = "data:" + (blob.type || "application/octet-stream") + ";base64," + btoa(s);
        } else this.result = decodeText(bytes);
        this.readyState = 2;
        this._fire("progress"); this._fire("load"); this._fire("loadend");
      });
    }
    _fire(type) {
      const ev = new ProgressEvent(type, { loaded: 0, total: 0 });
      const h = this["on" + type]; if (typeof h === "function") h.call(this, ev);
      this.dispatchEvent(ev);
    }
    readAsArrayBuffer(b) { this._read(b, "buffer"); }
    readAsBinaryString(b) { this._read(b, "binary"); }
    readAsDataURL(b) { this._read(b, "url"); }
    readAsText(b) { this._read(b, "text"); }
    abort() { if (this.readyState === 1) { this.readyState = 2; this.result = null; this._fire("abort"); this._fire("loadend"); } }
  }
  FileReader.EMPTY = 0; FileReader.LOADING = 1; FileReader.DONE = 2;

  class ProgressEvent extends Event {
    constructor(type, init = {}) {
      super(type, init);
      Object.defineProperties(this, { lengthComputable: { value: !!init.lengthComputable, enumerable: true },
        loaded: { value: Number(init.loaded || 0), enumerable: true }, total: { value: Number(init.total || 0), enumerable: true } });
    }
  }

  // A pull-based ReadableStream with a default reader: enough for response
  // bodies and libraries that read them chunk by chunk.
  const streamState = new WeakMap();
  const sstate = s => { const st = streamState.get(s); if (!st) throw new TypeError("Illegal ReadableStream invocation"); return st; };
  function streamPull(st) {
    if (st.pulling || st.closed || st.errored || !st.source.pull || (st.queue.length >= st.hwm && !st.reads.length)) return;
    st.pulling = true;
    Promise.resolve().then(() => st.source.pull(st.controller)).then(() => {
      st.pulling = false; if (st.reads.length) streamPull(st);
    }, e => streamError(st, e));
  }
  function streamFlush(st) {
    while (st.reads.length && st.queue.length) st.reads.shift().resolve({ value: st.queue.shift(), done: false });
    if (st.closed && !st.queue.length) while (st.reads.length) st.reads.shift().resolve({ value: undefined, done: true });
    if (st.reads.length) streamPull(st);
  }
  function streamError(st, e) {
    if (st.closed || st.errored) return;
    st.errored = true; st.error = e; st.queue = [];
    while (st.reads.length) st.reads.shift().reject(e);
  }
  class ReadableStream {
    constructor(source = {}, strategy = {}) {
      const st = { source, queue: [], reads: [], closed: false, errored: false, error: undefined, locked: false, pulling: false,
        hwm: strategy && strategy.highWaterMark !== undefined ? Number(strategy.highWaterMark) : 1 };
      st.controller = {
        enqueue(chunk) { if (st.closed) throw new TypeError("Cannot enqueue a chunk into a closed stream"); st.queue.push(chunk); streamFlush(st); },
        close() { st.closed = true; streamFlush(st); },
        error(e) { streamError(st, e); },
        get desiredSize() { return st.hwm - st.queue.length; },
      };
      streamState.set(this, st);
      try {
        Promise.resolve(source.start && source.start(st.controller)).then(() => streamPull(st), e => streamError(st, e));
      } catch (e) { streamError(st, e); }
    }
    get locked() { return sstate(this).locked; }
    getReader() {
      const st = sstate(this);
      if (st.locked) throw new TypeError("ReadableStream is locked");
      st.locked = true;
      let released = false;
      const closedP = new Promise((resolve, reject) => { st.onclose = resolve; st.onerror = reject; });
      return {
        read() {
          if (released) return Promise.reject(new TypeError("Reader released"));
          if (st.errored) return Promise.reject(st.error);
          return new Promise((resolve, reject) => { st.reads.push({ resolve, reject }); streamFlush(st); });
        },
        releaseLock() { released = true; st.locked = false; },
        cancel(reason) { st.closed = true; st.queue = []; streamFlush(st); return Promise.resolve(st.source.cancel && st.source.cancel(reason)); },
        get closed() { return closedP; },
      };
    }
    cancel(reason) {
      const st = sstate(this);
      if (st.locked) return Promise.reject(new TypeError("Cannot cancel a locked stream"));
      st.closed = true; st.queue = []; streamFlush(st);
      return Promise.resolve(st.source.cancel && st.source.cancel(reason)).then(() => undefined);
    }
    tee() {
      const reader = this.getReader(), branches = [];
      const pull = () => reader.read().then(r => { for (const c of branches) r.done ? c.close() : c.enqueue(r.value); });
      for (let i = 0; i < 2; i++) branches.push(null);
      return branches.map((_, i) => new ReadableStream({ start(c) { branches[i] = c; }, pull }));
    }
    async pipeTo(dest) {
      const reader = this.getReader(), writer = dest.getWriter();
      for (;;) { const r = await reader.read(); if (r.done) break; await writer.write(r.value); }
      await writer.close();
    }
    pipeThrough(pair) { this.pipeTo(pair.writable); return pair.readable; }
    [Symbol.asyncIterator]() {
      const reader = this.getReader();
      return { next: () => reader.read(), return: () => { reader.releaseLock(); return Promise.resolve({ done: true }); },
        [Symbol.asyncIterator]() { return this; } };
    }
  }
  ReadableStream.prototype.values = ReadableStream.prototype[Symbol.asyncIterator];

  /// A body's bytes from anything fetch and XMLHttpRequest accept, plus the
  /// content type it implies.
  function encodeBody(body) {
    if (body === null || body === undefined) return [null, null];
    if (body instanceof URLSearchParams) return [body.toString(), "application/x-www-form-urlencoded;charset=UTF-8"];
    if (body instanceof FormData) {
      return [new URLSearchParams([...body].map(([k, v]) => [k, v instanceof Blob ? "" : v])).toString(),
        "application/x-www-form-urlencoded;charset=UTF-8"];
    }
    if (body instanceof Blob) return [blobBytes(body), body.type || null];
    if (body instanceof ArrayBuffer || ArrayBuffer.isView(body)) return [bytesOf(body), null];
    return [String(body), "text/plain;charset=UTF-8"];
  }

  const absoluteURL = u => { try { return new URL(u, document.baseURI || location.href).href; } catch { return u; } };
  const bodyData = new WeakMap();
  class Body {
    _init(body, type) {
      const [bytes, implied] = encodeBody(body);
      bodyData.set(this, { bytes: bytes === null ? null : typeof bytes === "string" ? utf8(bytes) : bytes, used: false, stream: null });
      return implied;
    }
    get bodyUsed() { return bodyData.get(this).used; }
    get body() {
      const d = bodyData.get(this);
      if (d.bytes === null) return null;
      if (!d.stream) {
        // Disturbed only once read (merely looking at .body isn't using it).
        d.stream = new ReadableStream({ pull: c => { const b = d.bytes; d.used = true; if (b.length) c.enqueue(b); c.close(); } }, { highWaterMark: 0 });
      }
      return d.stream;
    }
    _take() {
      const d = bodyData.get(this);
      if (d.used) return Promise.reject(new TypeError("Body has already been consumed."));
      d.used = true;
      return Promise.resolve(d.bytes || new Uint8Array(0));
    }
    arrayBuffer() { return this._take().then(b => b.slice().buffer); }
    bytes() { return this._take().then(b => b.slice()); }
    blob() { return this._take().then(b => new Blob([b], { type: this.headers.get("content-type") || "" })); }
    text() { return this._take().then(b => decodeText(b, this.headers.get("content-type"))); }
    json() { return this.text().then(t => JSON.parse(t)); }
    formData() { return this.text().then(t => { const f = new FormData(); for (const [k, v] of new URLSearchParams(t)) f.append(k, v); return f; }); }
  }

  class Response extends Body {
    constructor(body = null, init = {}) {
      super();
      const implied = this._init(body);
      this.status = init.status ?? 200;
      this.statusText = init.statusText ?? "";
      this.headers = new Headers(init.headers);
      if (implied && !this.headers.has("content-type")) this.headers.set("content-type", implied);
      this.url = init.url ?? "";
      this.redirected = !!init.redirected;
      this.type = init.type || "basic";
    }
    get ok() { return this.status >= 200 && this.status < 300; }
    clone() {
      const d = bodyData.get(this);
      if (d.used) throw new TypeError("Response body is already used");
      return new Response(d.bytes, { status: this.status, statusText: this.statusText, headers: this.headers, url: this.url, type: this.type });
    }
    static json(data, init = {}) {
      return new Response(JSON.stringify(data), { ...init, headers: { "content-type": "application/json", ...(init.headers || {}) } });
    }
    static error() { return new Response(null, { status: 0, type: "error" }); }
    static redirect(url, status = 302) { return new Response(null, { status, headers: { location: String(url) } }); }
  }
  // A request's parts live in internal slots: fetch() reads those, never
  // the (overridable) properties, as browsers do.
  const requestSlots = new WeakMap();
  const reqSlot = r => { const s = requestSlots.get(r); if (!s) throw new TypeError("Illegal Request invocation"); return s; };
  class Request extends Body {
    constructor(input, init = {}) {
      super();
      const base = requestSlots.get(input) ? input : null;
      const b = base && reqSlot(base);
      const method = String(init.method || (b && b.method) || "GET").toUpperCase();
      const headers = new Headers(init.headers || (b && b.headers));
      const body = init.body !== undefined ? init.body : b ? bodyData.get(base).bytes : null;
      if (body !== null && body !== undefined && (method === "GET" || method === "HEAD"))
        throw new TypeError("Request with GET/HEAD method cannot have body.");
      const implied = this._init(body);
      if (implied && !headers.has("content-type")) headers.set("content-type", implied);
      requestSlots.set(this, {
        url: absoluteURL(b ? b.url : String(input)), method, headers,
        signal: init.signal || (b && b.signal) || new AbortSignal(),
        credentials: init.credentials || (b && b.credentials) || "same-origin",
        mode: init.mode || (b && b.mode) || "cors",
        cache: init.cache || (b && b.cache) || "default",
        redirect: init.redirect || (b && b.redirect) || "follow",
        referrer: init.referrer !== undefined ? String(init.referrer) : "about:client",
        referrerPolicy: init.referrerPolicy || "",
        integrity: init.integrity || "",
        keepalive: !!init.keepalive,
        priority: init.priority || "auto",
      });
    }
    clone() {
      if (this.bodyUsed) throw new TypeError("Request body is already used");
      return new Request(this);
    }
    get destination() { return ""; }
    get duplex() { return "half"; }
  }
  for (const key of ["url", "method", "headers", "signal", "credentials", "mode", "cache", "redirect", "referrer",
    "referrerPolicy", "integrity", "keepalive", "priority"]) {
    Object.defineProperty(Request.prototype, key, { configurable: true, enumerable: true, get() { return reqSlot(this)[key]; } });
  }
  /// The body of a `data:` URL, or null if it isn't one.
  function dataURL(url) {
    const r = __nebula_data_url(url);
    return r ? { bytes: r[0], type: r[1] } : null;
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

  function fetch(input, init = {}) {
    let req, slots;
    try { req = new Request(input, init); slots = reqSlot(req); } catch (e) { return Promise.reject(e); }
    const { url, method, headers, signal } = slots;
    if (signal && signal.aborted) return Promise.reject(signal.reason);
    const d = bodyData.get(req);
    if (d.used) return Promise.reject(new TypeError("Failed to execute 'fetch': Cannot construct a Request with a Request object that has already been used."));
    let pending;
    const data = url.startsWith("data:") ? dataURL(url) : null;
    if (url.startsWith("data:")) {
      pending = data ? Promise.resolve(new Response(method === "HEAD" ? null : data.bytes,
        { status: 200, statusText: "OK", headers: { "content-type": data.type }, url }))
        : Promise.reject(new TypeError("Failed to fetch"));
    } else {
      const body = method === "GET" || method === "HEAD" ? null : d.bytes;
      if (body) d.used = true;
      pending = request(method, url, [...headers], body).then(raw =>
        new Response(method === "HEAD" ? null : raw.body, { status: raw.status, statusText: raw.statusText,
          headers: raw.headers, url: raw.url, redirected: raw.url !== url }));
    }
    if (!signal) return pending;
    return new Promise((resolve, reject) => {
      signal.addEventListener("abort", () => reject(signal.reason));
      pending.then(resolve, reject);
    });
  }

  const XHR_EVENTS = ["readystatechange", "loadstart", "progress", "abort", "error", "load", "timeout", "loadend"];
  class XMLHttpRequestEventTarget extends EventTarget {}
  for (const type of XHR_EVENTS) {
    const key = "_on" + type;
    Object.defineProperty(XMLHttpRequestEventTarget.prototype, "on" + type, { configurable: true, enumerable: true,
      get() { return this[key] || null; }, set(f) { this[key] = typeof f === "function" ? f : null; } });
  }
  class XMLHttpRequestUpload extends XMLHttpRequestEventTarget {}
  class XMLHttpRequest extends XMLHttpRequestEventTarget {
    constructor() {
      super();
      this.readyState = 0;
      this.status = 0;
      this.statusText = "";
      this.responseURL = "";
      this.timeout = 0;
      this.withCredentials = false;
      this._type = "";
      this._raw = null;
      this._text = null;
      this._response = null;
      this._headers = [];
      this._responseHeaders = new Headers();
      this.upload = new XMLHttpRequestUpload();
    }
    get responseType() { return this._type; }
    set responseType(t) {
      t = String(t);
      if (["", "text", "json", "arraybuffer", "blob", "document"].includes(t)) this._type = t;
    }
    get responseText() {
      if (this._type !== "" && this._type !== "text")
        throw new DOMException("The value is only accessible if the object's 'responseType' is '' or 'text'.", "InvalidStateError");
      if (this.readyState < 3 || !this._raw) return "";
      if (this._text === null) this._text = decodeText(this._raw, this._mime || this._responseHeaders.get("content-type"));
      return this._text;
    }
    get response() {
      if (this._type === "" || this._type === "text") return this.responseText;
      if (this.readyState !== 4 || !this._raw) return null;
      if (this._response === null) {
        const type = this._mime || this._responseHeaders.get("content-type") || "";
        if (this._type === "arraybuffer") this._response = this._raw.slice().buffer;
        else if (this._type === "blob") this._response = new Blob([this._raw], { type });
        else if (this._type === "json") { try { this._response = JSON.parse(decodeText(this._raw, type)); } catch { this._response = null; } }
        else if (this._type === "document") this._response = new DOMParser().parseFromString(decodeText(this._raw, type), "text/html");
      }
      return this._response;
    }
    get responseXML() { return this._type === "document" ? this.response : null; }
    open(method, url, async = true) {
      this._method = String(method).toUpperCase();
      this._url = String(url);
      this._aborted = false;
      this._sent = false;
      this._headers = [];
      this._raw = null; this._text = null; this._response = null;
      this.status = 0; this.statusText = ""; this.responseURL = "";
      this._responseHeaders = new Headers();
      this._setState(1);
    }
    setRequestHeader(k, v) {
      if (this.readyState !== 1 || this._sent) throw new DOMException("The object's state must be OPENED.", "InvalidStateError");
      this._headers.push([String(k), String(v)]);
    }
    getResponseHeader(k) { return this.readyState < 2 ? null : this._responseHeaders.get(k); }
    getAllResponseHeaders() {
      return this.readyState < 2 ? "" : [...this._responseHeaders].map(([k, v]) => k + ": " + v + "\r\n").join("");
    }
    overrideMimeType(m) { this._mime = String(m); }
    _fire(type, target = this) {
      const n = this._raw ? this._raw.length : 0;
      const ev = type === "readystatechange" ? new Event(type) : new ProgressEvent(type, { lengthComputable: n > 0, loaded: n, total: n });
      target.dispatchEvent(ev);
      const h = target["on" + type];
      if (typeof h === "function") { try { h.call(target, ev); } catch (e) { reportError(e); } }
    }
    _setState(n) { this.readyState = n; this._fire("readystatechange"); }
    send(body) {
      if (this.readyState !== 1 || this._sent) throw new DOMException("The object's state must be OPENED.", "InvalidStateError");
      this._sent = true;
      const [bytes, type] = encodeBody(this._method === "GET" || this._method === "HEAD" ? null : body);
      if (type && !this._headers.some(([k]) => k.toLowerCase() === "content-type")) this._headers.push(["content-type", type]);
      this._fire("loadstart");
      const id = ++xhrSeq; this._id = id;
      if (this.timeout > 0) setTimeout(() => {
        if (this._id !== id || this.readyState === 4 || this._aborted) return;
        this._aborted = true; this.readyState = 4; this._fire("readystatechange"); this._fire("timeout"); this._fire("loadend");
      }, this.timeout);
      request(this._method, absoluteURL(this._url), this._headers, bytes).then(raw => {
        if (this._aborted || this._id !== id) return;
        this.status = raw.status;
        this.statusText = raw.statusText;
        this.responseURL = raw.url;
        this._responseHeaders = new Headers(raw.headers);
        this._setState(2);
        this._raw = new Uint8Array(raw.body);
        this._setState(3);
        this._fire("progress");
        this._setState(4);
        this._fire("load");
        this._fire("loadend");
      }, () => {
        if (this._aborted || this._id !== id) return;
        this.readyState = 4;
        this._fire("readystatechange");
        this._fire("error");
        this._fire("loadend");
      });
    }
    abort() {
      const active = this._sent && this.readyState !== 4 && !this._aborted;
      this._aborted = true;
      if (active) { this.readyState = 4; this._fire("readystatechange"); this._fire("abort"); this._fire("loadend"); }
      this.readyState = 0;
    }
  }
  let xhrSeq = 0;
  const reportError = e => { try { console.error("Uncaught " + (e && e.stack || e)); } catch {} };
  for (const [name, value] of Object.entries({ UNSENT: 0, OPENED: 1, HEADERS_RECEIVED: 2, LOADING: 3, DONE: 4 })) {
    Object.defineProperty(XMLHttpRequest, name, { value }); Object.defineProperty(XMLHttpRequest.prototype, name, { value });
  }

  for (const [name, value] of Object.entries({ Headers, Response, Request, AbortController, AbortSignal, FormData, XMLHttpRequest,
    XMLHttpRequestEventTarget, XMLHttpRequestUpload, fetch, Blob, File, FileReader, ProgressEvent, ReadableStream })) {
    define(name, value);
  }

  // ---------------------------------------------------------------- more DOM
  const elementProto = Element.prototype, nodeProto = Node.prototype;
  const nGet = elementProto.getAttribute, nSet = elementProto.setAttribute, nHas = elementProto.hasAttribute;
  const nRemove = elementProto.removeAttribute, nNames = elementProto.getAttributeNames;
  const setMethod = (proto, name, value) =>
    Object.defineProperty(proto, name, { configurable: true, writable: true, value });
  const getter = (proto, name, get, set) =>
    Object.defineProperty(proto, name, { configurable: true, enumerable: true, get, set });
  // Attributes keep their qualified names; a namespaced lookup matches the
  // local name with or without a prefix.
  function qualifiedName(el, local) {
    if (nHas.call(el, local)) return local;
    const names = nNames.call(el);
    return names.find(n => n.endsWith(":" + local)) || null;
  }
  setMethod(elementProto, "setAttributeNS", function setAttributeNS(ns, name, value) { nSet.call(this, String(name), value); });
  setMethod(elementProto, "getAttributeNS", function getAttributeNS(ns, local) {
    const q = qualifiedName(this, String(local)); return q === null ? null : nGet.call(this, q);
  });
  setMethod(elementProto, "hasAttributeNS", function hasAttributeNS(ns, local) { return qualifiedName(this, String(local)) !== null; });
  setMethod(elementProto, "removeAttributeNS", function removeAttributeNS(ns, local) {
    const q = qualifiedName(this, String(local)); if (q !== null) nRemove.call(this, q);
  });

  // Attr and NamedNodeMap: snapshots that write through to the element.
  const attrState = new WeakMap();
  class Attr {
    constructor(token) { if (token !== portToken) throw new TypeError("Illegal constructor"); }
    get name() { return attrState.get(this).name; }
    get localName() { const n = this.name; return n.slice(n.indexOf(":") + 1); }
    get prefix() { const n = this.name, i = n.indexOf(":"); return i < 0 ? null : n.slice(0, i); }
    get namespaceURI() { return this.prefix === "xlink" ? "http://www.w3.org/1999/xlink" : this.prefix === "xml" ? "http://www.w3.org/XML/1998/namespace" : null; }
    get value() { const s = attrState.get(this); return s.owner ? (nGet.call(s.owner, s.name) ?? s.value) : s.value; }
    set value(v) { const s = attrState.get(this); s.value = String(v); if (s.owner) nSet.call(s.owner, s.name, s.value); }
    get nodeValue() { return this.value; } set nodeValue(v) { this.value = v; }
    get textContent() { return this.value; } set textContent(v) { this.value = v; }
    get ownerElement() { return attrState.get(this).owner; }
    get specified() { return true; }
    get nodeType() { return 2; }
    get nodeName() { return this.name; }
    cloneNode() { return makeAttr(null, this.name, this.value); }
  }
  Object.setPrototypeOf(Attr.prototype, nodeProto);
  function makeAttr(owner, name, value) {
    const a = new Attr(portToken); attrState.set(a, { owner, name, value }); return a;
  }
  class NamedNodeMap {
    constructor(token) { if (token !== portToken) throw new TypeError("Illegal constructor"); }
    item(i) { return this[i] || null; }
    getNamedItem(name) { return Array.prototype.find.call(this, a => a.name === String(name).toLowerCase()) || null; }
    getNamedItemNS(ns, local) { return Array.prototype.find.call(this, a => a.localName === String(local)) || null; }
    setNamedItem(attr) { const owner = mapOwner.get(this); const old = this.getNamedItem(attr.name); nSet.call(owner, attr.name, attr.value); return old; }
    removeNamedItem(name) {
      const old = this.getNamedItem(name);
      if (!old) throw new DOMException("No item with that name", "NotFoundError");
      nRemove.call(mapOwner.get(this), old.name); return old;
    }
    [Symbol.iterator]() { return Array.prototype[Symbol.iterator].call(this); }
  }
  NamedNodeMap.prototype.setNamedItemNS = NamedNodeMap.prototype.setNamedItem;
  NamedNodeMap.prototype.removeNamedItemNS = function (ns, local) { const a = this.getNamedItemNS(ns, local); return this.removeNamedItem(a ? a.name : local); };
  const mapOwner = new WeakMap();
  getter(elementProto, "attributes", function () {
    const map = new NamedNodeMap(portToken), names = nNames.call(this);
    mapOwner.set(map, this);
    names.forEach((n, i) => Object.defineProperty(map, i, { value: makeAttr(this, n, nGet.call(this, n)), enumerable: true }));
    Object.defineProperty(map, "length", { value: names.length });
    return map;
  });
  setMethod(elementProto, "getAttributeNode", function (name) { name = String(name).toLowerCase(); return nHas.call(this, name) ? makeAttr(this, name, nGet.call(this, name)) : null; });
  setMethod(elementProto, "setAttributeNode", function (attr) { const old = this.getAttributeNode(attr.name); nSet.call(this, attr.name, attr.value); attrState.get(attr).owner = this; return old; });
  setMethod(elementProto, "removeAttributeNode", function (attr) { nRemove.call(this, attr.name); return attr; });
  setMethod(Document.prototype, "createAttribute", function (name) { return makeAttr(null, String(name).toLowerCase(), ""); });
  setMethod(Document.prototype, "createAttributeNS", function (ns, name) { return makeAttr(null, String(name), ""); });
  define("Attr", Attr); define("NamedNodeMap", NamedNodeMap);

  setMethod(elementProto, "hasAttributes", function hasAttributes() { return nNames.call(this).length > 0; });
  setMethod(elementProto, "webkitMatchesSelector", elementProto.matches);
  setMethod(elementProto, "msMatchesSelector", elementProto.matches);
  setMethod(elementProto, "replaceChildren", function replaceChildren(...nodes) {
    while (this.firstChild) this.removeChild(this.firstChild);
    this.append(...nodes);
  });
  if (!DocumentFragment.prototype.replaceChildren) setMethod(DocumentFragment.prototype, "replaceChildren", elementProto.replaceChildren);
  if (!elementProto.focus) setMethod(elementProto, "focus", function () { if (HTMLElement.prototype.focus) HTMLElement.prototype.focus.call(this); });
  if (!elementProto.blur) setMethod(elementProto, "blur", function () {});
  setMethod(elementProto, "requestFullscreen", function () { return Promise.reject(new TypeError("Fullscreen is not available")); });
  setMethod(elementProto, "checkVisibility", function () { const r = this.getBoundingClientRect(); return r.width > 0 || r.height > 0; });
  setMethod(elementProto, "hasPointerCapture", function () { return false; });
  setMethod(elementProto, "getAnimations", function () { return []; });
  getter(elementProto, "shadowRoot", function () { return null; });
  getter(elementProto, "assignedSlot", function () { return null; });
  getter(nodeProto, "baseURI", function () { return document.baseURI; });
  const docBase = () => { const b = document.querySelector("base[href]"); try { return b ? new URL(b.getAttribute("href"), location.href).href : location.href; } catch { return location.href; } };
  getter(Document.prototype, "baseURI", docBase);
  getter(Document.prototype, "URL", function () { return location.href; });
  getter(Document.prototype, "documentURI", function () { return location.href; });
  getter(Document.prototype, "compatMode", function () { return document.doctype ? "CSS1Compat" : "BackCompat"; });
  getter(Document.prototype, "characterSet", function () { return "UTF-8"; });
  getter(Document.prototype, "charset", function () { return "UTF-8"; });
  getter(Document.prototype, "inputEncoding", function () { return "UTF-8"; });
  getter(Document.prototype, "contentType", function () { return "text/html"; });
  getter(Document.prototype, "fullscreenEnabled", function () { return false; });
  getter(Document.prototype, "fullscreenElement", function () { return null; });
  getter(Document.prototype, "pictureInPictureEnabled", function () { return false; });
  getter(Document.prototype, "designMode", function () { return "off"; }, function () {});
  if (!("scrollingElement" in Document.prototype)) getter(Document.prototype, "scrollingElement", function () { return this.documentElement; });
  setMethod(Document.prototype, "execCommand", function () { return false; });
  setMethod(Document.prototype, "queryCommandSupported", function () { return false; });
  setMethod(Document.prototype, "exitFullscreen", function () { return Promise.resolve(); });
  setMethod(Document.prototype, "adoptNode", function (node) {
    if (node instanceof Document) throw new DOMException("Cannot adopt a document", "NotSupportedError");
    if (node.parentNode) node.parentNode.removeChild(node);
    return node;
  });
  for (const proto of [elementProto, HTMLElement.prototype]) {
    getter(proto, "tabIndex", function () {
      const v = parseInt(nGet.call(this, "tabindex"), 10);
      if (!isNaN(v)) return v;
      return /^(A|AREA|BUTTON|INPUT|SELECT|TEXTAREA|IFRAME)$/.test(this.tagName) ? 0 : -1;
    }, function (v) { nSet.call(this, "tabindex", String(v | 0)); });
  }
  getter(HTMLElement.prototype, "contentEditable", function () { return nGet.call(this, "contenteditable") ?? "inherit"; },
    function (v) { nSet.call(this, "contenteditable", String(v)); });
  getter(HTMLElement.prototype, "isContentEditable", function () { const v = nGet.call(this, "contenteditable"); return v === "" || v === "true"; });
  getter(HTMLElement.prototype, "draggable", function () { return nGet.call(this, "draggable") === "true"; }, function (v) { nSet.call(this, "draggable", String(!!v)); });
  getter(HTMLElement.prototype, "accessKey", function () { return nGet.call(this, "accesskey") || ""; }, function (v) { nSet.call(this, "accesskey", String(v)); });
  getter(HTMLElement.prototype, "inert", function () { return nHas.call(this, "inert"); }, function (v) { v ? nSet.call(this, "inert", "") : nRemove.call(this, "inert"); });
  getter(HTMLElement.prototype, "offsetParent", function () {
    if (!this.isConnected || getComputedStyle(this).display === "none") return null;
    for (let p = this.parentElement; p; p = p.parentElement) {
      if (p.tagName === "BODY") return p;
      if (getComputedStyle(p).position !== "static") return p;
    }
    return null;
  });

  // Tree comparisons and roots.
  setMethod(nodeProto, "getRootNode", function getRootNode() { let n = this; while (n.parentNode) n = n.parentNode; return n; });
  setMethod(nodeProto, "isSameNode", function isSameNode(other) { return this === other; });
  setMethod(nodeProto, "isEqualNode", function isEqualNode(other) {
    if (!other || this.nodeType !== other.nodeType || this.nodeName !== other.nodeName) return false;
    if (this.nodeType === 1) {
      const a = nNames.call(this), b = nNames.call(other);
      if (a.length !== b.length || a.some(n => nGet.call(other, n) !== nGet.call(this, n))) return false;
    } else if (this.nodeValue !== other.nodeValue) return false;
    const x = this.childNodes, y = other.childNodes;
    if (x.length !== y.length) return false;
    for (let i = 0; i < x.length; i++) if (!x[i].isEqualNode(y[i])) return false;
    return true;
  });
  setMethod(nodeProto, "compareDocumentPosition", function compareDocumentPosition(other) {
    if (this === other) return 0;
    const a = ancestors(this), b = ancestors(other);
    if (a[0] !== b[0]) return 1 | 32 | 2;
    let i = 0; while (i < a.length && i < b.length && a[i] === b[i]) i++;
    if (i === a.length) return 4 | 16;
    if (i === b.length) return 2 | 8;
    return nodeIndex(a[i]) < nodeIndex(b[i]) ? 4 : 2;
  });
  for (const [k, v] of Object.entries({ DOCUMENT_POSITION_DISCONNECTED: 1, DOCUMENT_POSITION_PRECEDING: 2, DOCUMENT_POSITION_FOLLOWING: 4,
    DOCUMENT_POSITION_CONTAINS: 8, DOCUMENT_POSITION_CONTAINED_BY: 16, DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC: 32 })) {
    Object.defineProperty(Node, k, { value: v }); Object.defineProperty(nodeProto, k, { value: v });
  }
  setMethod(nodeProto, "lookupNamespaceURI", function () { return this.namespaceURI || "http://www.w3.org/1999/xhtml"; });
  setMethod(nodeProto, "lookupPrefix", function () { return null; });
  setMethod(nodeProto, "isDefaultNamespace", function (ns) { return ns === "http://www.w3.org/1999/xhtml"; });
  setMethod(nodeProto, "normalize", function normalize() {
    for (let c = this.firstChild; c;) {
      const next = c.nextSibling;
      if (c.nodeType === 3) {
        if (c.data === "") this.removeChild(c);
        else while (c.nextSibling && c.nextSibling.nodeType === 3) { c.data += c.nextSibling.data; this.removeChild(c.nextSibling); }
      } else c.normalize();
      c = c.parentNode === this ? c.nextSibling : next;
    }
  });
  getter(Text.prototype, "wholeText", function () {
    let s = this; while (s.previousSibling && s.previousSibling.nodeType === 3) s = s.previousSibling;
    let t = ""; for (; s && s.nodeType === 3; s = s.nextSibling) t += s.data;
    return t;
  });
  getter(Text.prototype, "assignedSlot", function () { return null; });

  // Events: composed paths and the interfaces pages construct.
  setMethod(Event.prototype, "composedPath", function composedPath() {
    const t = this.target; if (!t || this.eventPhase === 0) return [];
    if (!(t instanceof Node)) return [t];
    const path = []; for (let n = t; n; n = n.parentNode) path.push(n);
    if (path[path.length - 1] === document) path.push(global);
    return path;
  });
  getter(Event.prototype, "composed", function () { return false; });
  getter(Event.prototype, "returnValue", function () { return !this.defaultPrevented; }, function (v) { if (!v) this.preventDefault(); });
  getter(Event.prototype, "cancelBubble", function () { return false; }, function (v) { if (v) this.stopPropagation(); });
  const eventClass = (name, parent, fields) => {
    const C = class extends parent {
      constructor(type, init = {}) {
        super(type, init);
        for (const [k, d] of Object.entries(fields))
          Object.defineProperty(this, k, { value: init[k] !== undefined ? init[k] : d, enumerable: true, configurable: true });
      }
    };
    Object.defineProperty(C, "name", { value: name });
    define(name, C);
    return C;
  };
  const UIEventC = global.UIEvent || Event, MouseEventC = global.MouseEvent || UIEventC;
  eventClass("ErrorEvent", Event, { message: "", filename: "", lineno: 0, colno: 0, error: null });
  eventClass("PromiseRejectionEvent", Event, { promise: null, reason: undefined });
  eventClass("PopStateEvent", Event, { state: null });
  eventClass("HashChangeEvent", Event, { oldURL: "", newURL: "" });
  eventClass("PageTransitionEvent", Event, { persisted: false });
  eventClass("TransitionEvent", Event, { propertyName: "", elapsedTime: 0, pseudoElement: "" });
  eventClass("AnimationEvent", Event, { animationName: "", elapsedTime: 0, pseudoElement: "" });
  eventClass("MessageEvent", Event, { data: null, origin: "", lastEventId: "", source: null, ports: [] });
  eventClass("CloseEvent", Event, { wasClean: false, code: 0, reason: "" });
  eventClass("SubmitEvent", Event, { submitter: null });
  eventClass("FormDataEvent", Event, { formData: null });
  eventClass("ClipboardEvent", Event, { clipboardData: null });
  eventClass("CompositionEvent", UIEventC, { data: "" });
  eventClass("DragEvent", MouseEventC, { dataTransfer: null });
  if (!global.StorageEvent) eventClass("StorageEvent", Event, { key: null, oldValue: null, newValue: null, url: "", storageArea: null });
  eventClass("PointerEvent", MouseEventC, { pointerId: 0, width: 1, height: 1, pressure: 0, tangentialPressure: 0,
    tiltX: 0, tiltY: 0, twist: 0, pointerType: "", isPrimary: false });
  eventClass("SecurityPolicyViolationEvent", Event, { violatedDirective: "", blockedURI: "" });

  // Parsing and serializing markup.
  class DOMParser {
    parseFromString(text, type) {
      const doc = document.implementation.createHTMLDocument("");
      const markup = String(text);
      if (/^\s*<!doctype|<html[\s>]/i.test(markup)) doc.documentElement.innerHTML = markup.replace(/^[\s\S]*?<html[^>]*>/i, "").replace(/<\/html>[\s\S]*$/i, "");
      else doc.body.innerHTML = markup;
      return doc;
    }
  }
  class XMLSerializer { serializeToString(node) { return node.outerHTML !== undefined ? node.outerHTML : node.nodeType === 3 ? node.data : node.textContent || ""; } }
  define("DOMParser", DOMParser); define("XMLSerializer", XMLSerializer);

  // CSS namespace.
  const CSSns = {
    supports(prop, value) {
      if (value === undefined) {
        const m = /^\s*\(?\s*([-\w]+)\s*:\s*([^)]+)\)?\s*$/.exec(String(prop));
        if (!m) return /^\s*selector\(/.test(String(prop));
        prop = m[1]; value = m[2];
      }
      const style = document.createElement("div").style;
      try { style.setProperty(String(prop), String(value)); return style.getPropertyValue(String(prop)) !== ""; } catch { return false; }
    },
    escape(s) {
      s = String(s); let out = "";
      for (let i = 0; i < s.length; i++) {
        const c = s.charCodeAt(i), ch = s[i];
        if (c === 0) out += "�";
        else if ((c >= 1 && c <= 31) || c === 127 || (i === 0 && c >= 48 && c <= 57) || (i === 1 && c >= 48 && c <= 57 && s[0] === "-"))
          out += "\\" + c.toString(16) + " ";
        else if (i === 0 && ch === "-" && s.length === 1) out += "\\-";
        else if (c >= 128 || ch === "-" || ch === "_" || /[0-9A-Za-z]/.test(ch)) out += ch;
        else out += "\\" + ch;
      }
      return out;
    },
    registerProperty() {},
  };
  define("CSS", CSSns);

  // Cloning values: the structured clone algorithm message channels use.
  define("structuredClone", function structuredClone(value, options) {
    if (arguments.length === 0) throw new TypeError("Failed to execute 'structuredClone': 1 argument required, but only 0 present.");
    if (value instanceof Blob) return value.slice(0, value.size, value.type);
    return cloneMessage(value);
  });
  // Weak references whose targets the page can always reach again: the
  // collector never runs finalizers, which no page can observe the absence of.
  class WeakRef {
    constructor(target) {
      if (target === null || (typeof target !== "object" && typeof target !== "function")) throw new TypeError("WeakRef: invalid target");
      Object.defineProperty(this, "_t", { value: target });
    }
    deref() { return this._t; }
    get [Symbol.toStringTag]() { return "WeakRef"; }
  }
  class FinalizationRegistry {
    constructor(cleanup) { if (typeof cleanup !== "function") throw new TypeError("FinalizationRegistry: cleanup must be callable"); }
    register(target) { if (target === null || (typeof target !== "object" && typeof target !== "function")) throw new TypeError("FinalizationRegistry.register: invalid target"); }
    unregister() { return false; }
    get [Symbol.toStringTag]() { return "FinalizationRegistry"; }
  }
  if (!global.WeakRef) define("WeakRef", WeakRef);
  if (!global.FinalizationRegistry) define("FinalizationRegistry", FinalizationRegistry);

  define("Option", function Option(text = "", value, defaultSelected = false, selected = false) {
    const o = document.createElement("option");
    if (text !== "") o.textContent = String(text);
    if (value !== undefined) o.setAttribute("value", String(value));
    if (defaultSelected) o.setAttribute("selected", "");
    if (selected) o.selected = true;
    return o;
  });
  // (No nodes may be made here: the page's document replaces this one.)
  global.Option.prototype = HTMLOptionElement.prototype;
  define("Audio", function Audio(src) {
    const a = document.createElement("audio"); a.setAttribute("preload", "auto");
    if (src !== undefined) a.setAttribute("src", String(src));
    return a;
  });
  global.Audio.prototype = HTMLAudioElement.prototype;

  // Navigator additions.
  const nav = global.navigator;
  const navDefine = (k, v) => { try { Object.defineProperty(nav, k, { value: v, configurable: true, enumerable: true }); } catch {} };
  navDefine("sendBeacon", function sendBeacon(url, data) {
    const [bytes, type] = encodeBody(data === undefined ? null : data);
    const headers = type ? [["content-type", type]] : [];
    request("POST", absoluteURL(String(url)), headers, bytes).catch(() => {});
    return true;
  });
  navDefine("mediaCapabilities", {
    decodingInfo(config) {
      const c = config && (config.video || config.audio);
      const type = c ? c.contentType : "";
      const supported = !!type && (typeof MediaSource !== "undefined" && MediaSource.isTypeSupported ? MediaSource.isTypeSupported(type) : document.createElement("video").canPlayType(type) !== "");
      return Promise.resolve({ supported, smooth: supported, powerEfficient: false });
    },
    encodingInfo() { return Promise.resolve({ supported: false, smooth: false, powerEfficient: false }); },
  });
  navDefine("getGamepads", () => []);
  navDefine("javaEnabled", () => false);
  navDefine("pdfViewerEnabled", false);
  navDefine("webdriver", false);
  navDefine("doNotTrack", null);
  navDefine("plugins", Object.assign([], { item: () => null, namedItem: () => null, refresh() {} }));
  navDefine("mimeTypes", Object.assign([], { item: () => null, namedItem: () => null }));

  // HTMLHyperlinkElementUtils: <a> and <area> expose their URL's parts.
  for (const proto of [HTMLAnchorElement.prototype, global.HTMLAreaElement && HTMLAreaElement.prototype].filter(Boolean)) {
    const url = el => { const h = nGet.call(el, "href"); if (h === null) return null; try { return new URL(h, document.baseURI); } catch { return null; } };
    getter(proto, "href", function () { const u = url(this); return u ? u.href : (nGet.call(this, "href") || ""); },
      function (v) { nSet.call(this, "href", String(v)); });
    for (const part of ["protocol", "username", "password", "host", "hostname", "port", "pathname", "search", "hash"]) {
      getter(proto, part, function () { const u = url(this); return u ? u[part] : ""; },
        function (v) { const u = url(this); if (!u) return; u[part] = v; nSet.call(this, "href", u.href); });
    }
    getter(proto, "origin", function () { const u = url(this); return u ? u.origin : ""; });
    setMethod(proto, "toString", function toString() { return this.href; });
    for (const attr of ["download", "ping", "hreflang", "referrerPolicy"])
      getter(proto, attr, function () { return nGet.call(this, attr.toLowerCase()) || ""; }, function (v) { nSet.call(this, attr.toLowerCase(), String(v)); });
  }

  // ---------------------------------------------------------------- media source extensions
  const mse = global.__nebula_mse;
  class TimeRanges {
    constructor(token, flat) {
      if (token !== portToken) throw new TypeError("Illegal constructor");
      Object.defineProperty(this, "_r", { value: flat });
    }
    get length() { return this._r.length / 2; }
    start(i) {
      if (!(i >= 0 && i < this.length)) throw new DOMException("The index provided (" + i + ") is greater than or equal to the maximum bound (" + this.length + ").", "IndexSizeError");
      return this._r[2 * i];
    }
    end(i) {
      if (!(i >= 0 && i < this.length)) throw new DOMException("The index provided (" + i + ") is greater than or equal to the maximum bound (" + this.length + ").", "IndexSizeError");
      return this._r[2 * i + 1];
    }
  }
  define("TimeRanges", TimeRanges);
  const timeRanges = flat => new TimeRanges(portToken, flat);

  const sourceIds = new WeakMap(), sourceById = new Map();
  const msId = ms => { const id = sourceIds.get(ms); if (id === undefined) throw new TypeError("Illegal MediaSource invocation"); return id; };
  const fire = (target, type) => {
    const ev = new Event(type);
    const h = target["on" + type];
    target.dispatchEvent(ev);
    if (typeof h === "function") { try { h.call(target, ev); } catch (e) { reportError(e); } }
  };
  const task = f => setTimeout(f, 0);
  const handlerProps = (proto, names) => {
    for (const type of names) {
      const key = "_on" + type;
      Object.defineProperty(proto, "on" + type, { configurable: true, enumerable: true,
        get() { return this[key] || null; }, set(f) { this[key] = typeof f === "function" ? f : null; } });
    }
  };

  class SourceBufferList extends EventTarget {
    constructor(token) { super(); if (token !== portToken) throw new TypeError("Illegal constructor"); Object.defineProperty(this, "_list", { value: [] }); }
    get length() { return this._list.length; }
    _sync() {
      for (let i = 0; i < Math.max(this._list.length, this._shown || 0); i++) {
        if (i < this._list.length) Object.defineProperty(this, i, { value: this._list[i], configurable: true, enumerable: true });
        else delete this[i];
      }
      this._shown = this._list.length;
    }
    [Symbol.iterator]() { return this._list[Symbol.iterator](); }
  }
  handlerProps(SourceBufferList.prototype, ["addsourcebuffer", "removesourcebuffer"]);

  const bufferState = new WeakMap();
  const sbState = sb => { const s = bufferState.get(sb); if (!s) throw new TypeError("Illegal SourceBuffer invocation"); return s; };
  class SourceBuffer extends EventTarget {
    constructor(token) { super(); if (token !== portToken) throw new TypeError("Illegal constructor"); }
    get updating() { return sbState(this).updating; }
    get buffered() {
      const s = sbState(this);
      if (s.removed) throw new DOMException("This SourceBuffer has been removed from the parent media source.", "InvalidStateError");
      return timeRanges(mse.buffered(s.ms, s.id));
    }
    get mode() { return sbState(this).mode; }
    set mode(v) {
      const s = sbState(this); v = String(v);
      if (v !== "segments" && v !== "sequence") throw new TypeError("Invalid mode");
      if (s.updating) throw new DOMException("updating", "InvalidStateError");
      s.mode = v; mse.set(s.ms, s.id, "mode", v);
    }
    get timestampOffset() { return sbState(this).timestampOffset; }
    set timestampOffset(v) {
      const s = sbState(this); v = Number(v);
      if (s.updating) throw new DOMException("The SourceBuffer is updating.", "InvalidStateError");
      s.timestampOffset = v; mse.set(s.ms, s.id, "timestampOffset", v);
    }
    get appendWindowStart() { return sbState(this).windowStart; }
    set appendWindowStart(v) {
      const s = sbState(this); v = Number(v);
      if (!(v >= 0) || v >= s.windowEnd) throw new TypeError("Invalid appendWindowStart");
      s.windowStart = v; mse.set(s.ms, s.id, "appendWindowStart", v);
    }
    get appendWindowEnd() { return sbState(this).windowEnd; }
    set appendWindowEnd(v) {
      const s = sbState(this); v = Number(v);
      if (Number.isNaN(v) || v <= s.windowStart) throw new TypeError("Invalid appendWindowEnd");
      s.windowEnd = v; mse.set(s.ms, s.id, "appendWindowEnd", v);
    }
    get audioTracks() { return []; }
    get videoTracks() { return []; }
    get textTracks() { return []; }
    _check() {
      const s = sbState(this);
      if (s.removed) throw new DOMException("This SourceBuffer has been removed from the parent media source.", "InvalidStateError");
      if (s.updating) throw new DOMException("This SourceBuffer is still processing an 'appendBuffer' or 'remove' operation.", "InvalidStateError");
      return s;
    }
    _run(work) {
      const s = sbState(this);
      s.updating = true;
      const ms = sourceById.get(s.ms);
      if (ms && ms.readyState === "ended") { mse.setReady(s.ms, "open"); ms._fire("sourceopen"); }
      task(() => {
        if (!s.updating) return;
        fire(this, "updatestart");
        const error = work();
        s.updating = false;
        if (error) {
          fire(this, "error"); fire(this, "updateend");
          if (error === "decode" && ms) ms.endOfStream("decode");
        } else { fire(this, "update"); fire(this, "updateend"); }
      });
    }
    appendBuffer(data) {
      const s = this._check();
      if (!(data instanceof ArrayBuffer || ArrayBuffer.isView(data)))
        throw new TypeError("Failed to execute 'appendBuffer' on 'SourceBuffer': The provided value is not of type '(ArrayBuffer or ArrayBufferView)'.");
      const ms = sourceById.get(s.ms);
      if (ms && ms.readyState === "closed") throw new DOMException("The MediaSource is closed.", "InvalidStateError");
      const bytes = bytesOf(data);
      // The quota check is synchronous, as in browsers.
      const result = mse.append(s.ms, s.id, new Uint8Array(0));
      if (result === "QuotaExceededError") throw new DOMException("The SourceBuffer is full, and cannot free space to append additional buffers.", "QuotaExceededError");
      this._run(() => {
        const r = mse.append(s.ms, s.id, bytes);
        if (r === "QuotaExceededError") { console.warn("SourceBuffer quota exceeded"); return "quota"; }
        if (s.ms && ms && ms._element) ms._element._mediaPoke && ms._element._mediaPoke();
        return r;
      });
    }
    appendBufferAsync(data) {
      return new Promise((resolve, reject) => {
        const done = () => { this.removeEventListener("updateend", done); resolve(); };
        this.addEventListener("updateend", done);
        try { this.appendBuffer(data); } catch (e) { this.removeEventListener("updateend", done); reject(e); }
      });
    }
    remove(start, end) {
      const s = this._check();
      start = Number(start); end = Number(end);
      if (!(start >= 0) || !(end > start)) throw new TypeError("Failed to execute 'remove' on 'SourceBuffer': The start or end provided is invalid.");
      this._run(() => { mse.remove(s.ms, s.id, start, end); });
    }
    abort() {
      const s = sbState(this);
      if (s.removed) throw new DOMException("This SourceBuffer has been removed.", "InvalidStateError");
      if (s.updating) { s.updating = false; fire(this, "abort"); fire(this, "updateend"); }
      mse.abort(s.ms, s.id);
      s.windowStart = 0; s.windowEnd = Infinity;
      mse.set(s.ms, s.id, "appendWindowStart", 0); mse.set(s.ms, s.id, "appendWindowEnd", Infinity);
    }
    changeType(type) {
      const s = this._check();
      if (!mse.isTypeSupported(String(type))) throw new DOMException("The type provided ('" + type + "') is unsupported.", "NotSupportedError");
    }
  }
  handlerProps(SourceBuffer.prototype, ["updatestart", "update", "updateend", "error", "abort"]);

  class MediaSource extends EventTarget {
    constructor() {
      super();
      const id = mse.create();
      sourceIds.set(this, id); sourceById.set(id, this);
      Object.defineProperties(this, {
        sourceBuffers: { value: new SourceBufferList(portToken), enumerable: true },
        activeSourceBuffers: { value: new SourceBufferList(portToken), enumerable: true },
        _element: { value: null, writable: true },
      });
    }
    static isTypeSupported(type) { return mse.isTypeSupported(String(type)); }
    static get canConstructInDedicatedWorker() { return false; }
    get readyState() { return mse.get(msId(this)).readyState; }
    get duration() { return this.readyState === "closed" ? NaN : mse.get(msId(this)).duration; }
    set duration(v) {
      v = Number(v);
      if (Number.isNaN(v) || v < 0) throw new TypeError("Failed to set the 'duration' property on 'MediaSource': The value provided is invalid.");
      if (this.readyState !== "open") throw new DOMException("The MediaSource's readyState is not 'open'.", "InvalidStateError");
      if (this.sourceBuffers._list.some(b => b.updating)) throw new DOMException("A SourceBuffer is updating.", "InvalidStateError");
      mse.setDuration(msId(this), v);
      if (this._element) task(() => fire(this._element, "durationchange"));
    }
    addSourceBuffer(type) {
      type = String(type);
      if (!type) throw new TypeError("Failed to execute 'addSourceBuffer' on 'MediaSource': The type provided is empty.");
      const r = mse.addSourceBuffer(msId(this), type);
      if (typeof r === "string") {
        const messages = { NotSupportedError: "The type provided ('" + type + "') is unsupported.",
          InvalidStateError: "The MediaSource's readyState is not 'open'.", QuotaExceededError: "This MediaSource has reached the limit of SourceBuffer objects it can handle." };
        throw new DOMException(messages[r] || r, r);
      }
      const sb = new SourceBuffer(portToken);
      bufferState.set(sb, { ms: msId(this), id: r, updating: false, removed: false, mode: "segments",
        timestampOffset: 0, windowStart: 0, windowEnd: Infinity });
      for (const list of [this.sourceBuffers, this.activeSourceBuffers]) { list._list.push(sb); list._sync(); }
      task(() => { fire(this.sourceBuffers, "addsourcebuffer"); fire(this.activeSourceBuffers, "addsourcebuffer"); });
      return sb;
    }
    removeSourceBuffer(sb) {
      const s = bufferState.get(sb);
      if (!s || s.ms !== msId(this) || s.removed) throw new DOMException("The SourceBuffer provided is not contained in this MediaSource.", "NotFoundError");
      if (s.updating) { s.updating = false; task(() => { fire(sb, "abort"); fire(sb, "updateend"); }); }
      s.removed = true;
      mse.removeSourceBuffer(s.ms, s.id);
      for (const list of [this.sourceBuffers, this.activeSourceBuffers]) {
        list._list.splice(list._list.indexOf(sb), 1); list._sync();
      }
      task(() => { fire(this.sourceBuffers, "removesourcebuffer"); fire(this.activeSourceBuffers, "removesourcebuffer"); });
    }
    endOfStream(error) {
      if (this.readyState !== "open") throw new DOMException("The MediaSource's readyState is not 'open'.", "InvalidStateError");
      if (this.sourceBuffers._list.some(b => b.updating)) throw new DOMException("A SourceBuffer is updating.", "InvalidStateError");
      mse.setReady(msId(this), "ended");
      if (!error) {
        // The duration becomes the end of the buffered media.
        const el = this._element, b = el && el.buffered;
        if (b && b.length) mse.setDuration(msId(this), Math.max(b.end(b.length - 1), 0));
      }
      task(() => fire(this, "sourceended"));
      if (error === "decode" && this._element) this._element._mediaFail("The media could not be decoded.");
    }
    setLiveSeekableRange(start, end) { this._live = [Number(start), Number(end)]; }
    clearLiveSeekableRange() { this._live = null; }
    get handle() { return null; }
    _fire(type) { fire(this, type); }
  }
  handlerProps(MediaSource.prototype, ["sourceopen", "sourceended", "sourceclose"]);
  define("MediaSource", MediaSource);
  define("SourceBuffer", SourceBuffer);
  define("SourceBufferList", SourceBufferList);

  // Object URLs: blobs (their bytes are copied to the browser) and media sources.
  const objectSources = new Map();
  Object.defineProperties(URL, {
    createObjectURL: { configurable: true, writable: true, value: function createObjectURL(obj) {
      if (obj instanceof MediaSource) { const u = mse.objectURL("mediasource", msId(obj)); objectSources.set(u, obj); return u; }
      if (obj instanceof Blob) return mse.objectURL("blob", blobBytes(obj), obj.type);
      throw new TypeError("Failed to execute 'createObjectURL' on 'URL': Overload resolution failed.");
    } },
    revokeObjectURL: { configurable: true, writable: true, value: function revokeObjectURL(url) {
      url = String(url); objectSources.delete(url); mse.revokeObjectURL(url);
    } },
  });
  const fetchNetwork = fetch;
  const fetchBlob = function fetch(input, init) {
    const url = requestSlots.get(input) ? reqSlot(input).url : String(input);
    if (url.startsWith("blob:")) {
      const found = mse.blobURL(url);
      if (!found) return Promise.reject(new TypeError("Failed to fetch"));
      return Promise.resolve(new Response(found[0], { status: 200, statusText: "OK", headers: { "content-type": found[1] }, url }));
    }
    return fetchNetwork(input, init);
  };
  define("fetch", fetchBlob);

  // Attaching a MediaSource to a media element (src or srcObject).
  const attached = new WeakMap();
  function attachSource(el, ms) {
    const old = attached.get(el);
    if (old === ms) return;
    if (old) detachSource(el);
    if (!ms) return;
    if (ms.readyState !== "closed") { el._mediaFail("The MediaSource is already attached."); return; }
    attached.set(el, ms);
    ms._element = el;
    mse.attach(msId(ms), el);
    task(() => { if (attached.get(el) === ms) ms._fire("sourceopen"); });
  }
  function detachSource(el) {
    const ms = attached.get(el);
    if (!ms) return;
    attached.delete(el);
    ms._element = null;
    for (const sb of ms.sourceBuffers._list) { const s = bufferState.get(sb); if (s) { s.removed = true; s.updating = false; } }
    for (const list of [ms.sourceBuffers, ms.activeSourceBuffers]) { list._list.length = 0; list._sync(); }
    mse.detach(msId(ms));
    task(() => ms._fire("sourceclose"));
  }
  const mediaProto = HTMLMediaElement.prototype;
  const setSrc = (el, value) => {
    const url = String(value);
    settlePlays(el, new DOMException("The play() request was interrupted by a new load request.", "AbortError"));
    el.setAttribute("src", url);
    attachSource(el, objectSources.get(url) || null);
    el._mediaCommand("load");
    task(() => fire(el, "emptied"));
    if (!objectSources.has(url) && url) task(() => fire(el, "loadstart"));
  };
  Object.defineProperties(mediaProto, {
    src: { configurable: true, enumerable: true,
      get() { const v = this.getAttribute("src"); return v === null ? "" : absoluteURL(v); },
      set(v) { setSrc(this, v); } },
    srcObject: { configurable: true, enumerable: true,
      get() { return attached.get(this) || this._srcObject || null; },
      set(v) {
        this._srcObject = v || null;
        if (v instanceof MediaSource) { this.removeAttribute("src"); attachSource(this, v); this._mediaCommand("load"); }
        else if (v instanceof Blob) setSrc(this, URL.createObjectURL(v));
        else { detachSource(this); }
      } },
    buffered: { configurable: true, enumerable: true, get() { return timeRanges(this._mediaBuffered()); } },
    seekable: { configurable: true, enumerable: true, get() {
      const ms = attached.get(this);
      if (ms && ms._live) return timeRanges(ms._live.slice());
      const d = this.duration;
      if (Number.isNaN(d) || this.readyState < 1) return timeRanges([]);
      if (d === Infinity) { const b = this.buffered; return timeRanges(b.length ? [b.start(0), b.end(b.length - 1)] : []); }
      return timeRanges([0, d]);
    } },
    played: { configurable: true, enumerable: true, get() { return timeRanges([]); } },
    networkState: { configurable: true, enumerable: true, get() {
      return !this.getAttribute("src") && !attached.get(this) ? 0 : this.readyState >= 4 ? 1 : 2;
    } },
    playbackRate: { configurable: true, enumerable: true, get() { return this._mediaState().playbackRate; },
      set(v) { v = Number(v); if (!Number.isFinite(v)) throw new TypeError("Invalid playbackRate"); this._mediaCommand("rate", v); task(() => fire(this, "ratechange")); } },
    defaultPlaybackRate: { configurable: true, enumerable: true, get() { return this._defaultRate || 1; }, set(v) { this._defaultRate = Number(v); } },
    preservesPitch: { configurable: true, enumerable: true, get() { return true; }, set() {} },
    preload: { configurable: true, enumerable: true, get() { return this.getAttribute("preload") || "auto"; }, set(v) { this.setAttribute("preload", String(v)); } },
    crossOrigin: { configurable: true, enumerable: true, get() { return this.getAttribute("crossorigin"); },
      set(v) { v === null ? this.removeAttribute("crossorigin") : this.setAttribute("crossorigin", String(v)); } },
    mediaKeys: { configurable: true, enumerable: true, get() { return null; } },
    textTracks: { configurable: true, enumerable: true, get() {
      return this._textTracks || (this._textTracks = Object.assign([], { addEventListener() {}, removeEventListener() {}, getTrackById: () => null, onaddtrack: null, onremovetrack: null, onchange: null }));
    } },
    audioTracks: { configurable: true, enumerable: true, get() { return Object.assign([], { addEventListener() {}, removeEventListener() {}, getTrackById: () => null }); } },
    videoTracks: { configurable: true, enumerable: true, get() { return Object.assign([], { addEventListener() {}, removeEventListener() {}, getTrackById: () => null }); } },
    addTextTrack: { configurable: true, writable: true, value(kind, label = "", language = "") {
      const track = { kind, label, language, mode: "hidden", cues: [], activeCues: [], addCue(c) { this.cues.push(c); }, removeCue(c) { const i = this.cues.indexOf(c); if (i >= 0) this.cues.splice(i, 1); },
        addEventListener() {}, removeEventListener() {} };
      this.textTracks.push(track);
      return track;
    } },
    setMediaKeys: { configurable: true, writable: true, value() { return Promise.reject(new DOMException("Encrypted media is not supported", "NotSupportedError")); } },
    fastSeek: { configurable: true, writable: true, value(t) { this.currentTime = t; } },
    getStartDate: { configurable: true, writable: true, value() { return new Date(NaN); } },
    setSinkId: { configurable: true, writable: true, value() { return Promise.resolve(); } },
    sinkId: { configurable: true, enumerable: true, get() { return ""; } },
    _mediaFail: { configurable: true, writable: true, value(message) {
      this._mediaCommand("pause");
      this._failMessage = message;
      task(() => fire(this, "error"));
    } },
  });
  const loadNative = mediaProto.load;
  Object.defineProperty(mediaProto, "load", { configurable: true, writable: true, value: function load() {
    const src = this.getAttribute("src");
    attachSource(this, src ? objectSources.get(src) || null : (this._srcObject instanceof MediaSource ? this._srcObject : null));
    return loadNative.call(this);
  } });
  const removeAttr = elementProto.removeAttribute;
  // Removing src detaches its media source when the element next loads.
  Object.defineProperty(HTMLVideoElement.prototype, "getVideoPlaybackQuality", { configurable: true, writable: true, value() {
    const s = this._mediaState();
    return { creationTime: performance.now(), totalVideoFrames: Math.round(s.currentTime * 30), droppedVideoFrames: 0, corruptedVideoFrames: 0, totalFrameDelay: 0 };
  } });
  for (const name of ["poster"]) Object.defineProperty(HTMLVideoElement.prototype, name, { configurable: true, enumerable: true,
    get() { const v = this.getAttribute(name); return v === null ? "" : absoluteURL(v); }, set(v) { this.setAttribute(name, String(v)); } });
  for (const name of ["width", "height"]) Object.defineProperty(HTMLVideoElement.prototype, name, { configurable: true, enumerable: true,
    get() { return parseInt(this.getAttribute(name), 10) || 0; }, set(v) { this.setAttribute(name, String(v >>> 0)); } });
  for (const name of ["playsInline", "disablePictureInPicture", "disableRemotePlayback"]) {
    const attr = name.toLowerCase();
    Object.defineProperty(HTMLVideoElement.prototype, name, { configurable: true, enumerable: true,
      get() { return this.hasAttribute(attr); }, set(v) { v ? this.setAttribute(attr, "") : removeAttr.call(this, attr); } });
  }
  Object.defineProperty(mediaProto, "canPlayType", { configurable: true, writable: true, value(type) {
    type = String(type).trim();
    if (!type) return "";
    return mse.isTypeSupported(type) ? (/codecs\s*=/.test(type) ? "probably" : "maybe") : "";
  } });
  for (const [name, value] of Object.entries({ NETWORK_EMPTY: 0, NETWORK_IDLE: 1, NETWORK_LOADING: 2, NETWORK_NO_SOURCE: 3 })) {
    Object.defineProperty(HTMLMediaElement, name, { value }); Object.defineProperty(mediaProto, name, { value });
  }
  class MediaError {
    constructor(token, code, message) { if (token !== portToken) throw new TypeError("Illegal constructor"); this.code = code; this.message = message; }
  }
  for (const [name, value] of Object.entries({ MEDIA_ERR_ABORTED: 1, MEDIA_ERR_NETWORK: 2, MEDIA_ERR_DECODE: 3, MEDIA_ERR_SRC_NOT_SUPPORTED: 4 })) {
    Object.defineProperty(MediaError, name, { value }); Object.defineProperty(MediaError.prototype, name, { value });
  }
  define("MediaError", MediaError);
  Object.defineProperty(mediaProto, "error", { configurable: true, enumerable: true, get() {
    const message = this._mediaState().error || this._failMessage;
    if (!message) return null;
    return new MediaError(portToken, /decod/i.test(message) ? 3 : /network|request failed/i.test(message) ? 2 : 4, message);
  } });

  // ---------------------------------------------------------------- text and crypto
  class TextEncoder {
    get encoding() { return "utf-8"; }
    encode(input = "") {
      return __nebula_utf8(String(input));
    }
    encodeSlow(input = "") {
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
