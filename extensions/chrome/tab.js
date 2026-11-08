// Driving a tab the operator allowed. The grant IS the chrome.debugger attachment, so Chrome's
// "is debugging this browser" bar is up exactly while a session may act. Commands come from the
// bridge, run on that tab only, and are a fixed list: no script from the session ever runs here.
// An ask also lets its session look (shot, read) at the asked tab, without the debugger: the
// ask's gesture granted activeTab on it, which lasts until the tab navigates.

const GRANTS = "tabGrants";
const LOAD_MS = 15000;
const SETTLE_MS = 600;
const TEXT_MAX = 64 * 1024;
const ELEMENTS_MAX = 150;

let deps = null;
let polling = false;

// "tabId:session" -> open asks. In memory: a restarted worker has lost those streams too.
const looks = new Map();

export function lookStart(tabId, session) {
  const k = tabId + ":" + session;
  looks.set(k, (looks.get(k) || 0) + 1);
}

export function lookEnd(tabId, session) {
  const k = tabId + ":" + session;
  const n = (looks.get(k) || 0) - 1;
  if (n > 0) looks.set(k, n); else looks.delete(k);
}

export function poll() { loop(); }

// {tabId: {session, name, url, title}}, in storage.session so a restarted worker keeps it.
export async function grants() {
  return (await chrome.storage.session.get(GRANTS))[GRANTS] || {};
}

async function save(g) {
  await chrome.storage.session.set({ [GRANTS]: g });
}

function wire(g) {
  return Object.entries(g).map(([tab, v]) => ({ session: v.session, tab: Number(tab), url: v.url, title: v.title }));
}

// Tell the bridge; detach whatever it refused, so no bar stays up for a grant nobody can use.
async function sync() {
  const g = await grants();
  const r = await (await deps.bridge("/tab/sync", post({ grants: wire(g) }))).json();
  return dropRefused(r);
}

async function dropRefused(r) {
  const refused = r.refused || [];
  if (!refused.length) return [];
  const g = await grants();
  for (const { session } of refused) {
    for (const [tab, v] of Object.entries(g)) {
      if (v.session === session) { delete g[tab]; detach(Number(tab)); }
    }
  }
  await save(g);
  return refused;
}

function post(body) {
  return { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) };
}

function detach(tabId) {
  chrome.debugger.detach({ tabId }).catch(() => {});
}

export async function stateOf(tabId) {
  return (await grants())[tabId] || null;
}

export async function allow(tabId, session, name) {
  const tab = await chrome.tabs.get(tabId);
  if (!/^https?:/i.test(tab.url || "")) throw new Error("Only an http or https page can be driven.");
  const g = await grants();
  // One tab per session: allowing a second moves the grant.
  for (const [t, v] of Object.entries(g)) {
    if (v.session === session && Number(t) !== tabId) { delete g[t]; detach(Number(t)); }
  }
  if (!g[tabId]) await chrome.debugger.attach({ tabId }, "1.3");
  g[tabId] = { session, name, url: tab.url, title: tab.title };
  await save(g);
  let refused;
  try {
    refused = await sync();
  } catch (e) {
    await revoke(tabId);
    throw e;
  }
  if (refused.length) throw new Error(refused[0].why);
  loop();
}

export async function revoke(tabId) {
  const g = await grants();
  if (!g[tabId]) return;
  delete g[tabId];
  await save(g);
  detach(tabId);
  await sync().catch(() => {});
}

// Operator cancelled the bar, the tab closed, or Chrome took the debugger away: the grant ends.
async function gone(tabId) {
  const g = await grants();
  if (!g[tabId]) return;
  delete g[tabId];
  await save(g);
  await sync().catch(() => {});
}

// A restarted worker keeps only the grants Chrome still has attached.
async function resume() {
  const g = await grants();
  if (!Object.keys(g).length) return;
  const attached = new Set((await chrome.debugger.getTargets()).filter((t) => t.attached && t.tabId !== undefined).map((t) => t.tabId));
  for (const tab of Object.keys(g)) if (!attached.has(Number(tab))) delete g[tab];
  await save(g);
  if (Object.keys(g).length) loop();
}

export function setupTabs(d) {
  deps = d;
  chrome.debugger.onDetach.addListener((src) => { if (src.tabId !== undefined) gone(src.tabId); });
  chrome.tabs.onRemoved.addListener((tabId) => gone(tabId));
  resume().catch(() => {});
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// The long poll, alive while any grant is. Each poll carries the grant list, so a restarted
// bridge learns it again within one round.
async function loop() {
  if (polling) return;
  polling = true;
  deps.keepaliveStart();
  try {
    for (;;) {
      const g = await grants();
      if (!Object.keys(g).length && !looks.size) break;
      let r;
      const at = Date.now();
      try {
        r = await (await deps.bridge("/tab/next", post({ grants: wire(g) }))).json();
      } catch (_) {
        await sleep(3000);
        continue;
      }
      await dropRefused(r);
      // The bridge answers at once while it holds no grant yet (an ask not posted): no busy loop.
      if (!r.command && Date.now() - at < 1000) await sleep(500);
      if (!r.command) continue;
      const cmd = r.command;
      let body;
      try {
        body = { id: cmd.id, ok: true, result: await run(cmd) };
      } catch (e) {
        body = { id: cmd.id, ok: false, error: e.message || String(e) };
      }
      await deps.bridge("/tab/result", post(body)).catch(() => {});
    }
  } finally {
    polling = false;
    deps.keepaliveStop();
  }
}

// ---------------------------------------------------------------- the acts

const cdp = (tabId, method, params = {}) => chrome.debugger.sendCommand({ tabId }, method, params);

// A fixed function of ours, run in an isolated world: the page's own script cannot see or patch it.
async function inPage(tabId, { func, args = [] }) {
  const tree = await cdp(tabId, "Page.getFrameTree");
  const world = await cdp(tabId, "Page.createIsolatedWorld", { frameId: tree.frameTree.frame.id, worldName: "letibot" });
  const r = await cdp(tabId, "Runtime.evaluate", {
    expression: `(${func})(...${JSON.stringify(args)})`,
    contextId: world.executionContextId, returnByValue: true, awaitPromise: true,
  });
  if (r.exceptionDetails) {
    const ex = r.exceptionDetails.exception;
    throw new Error("the page step failed: " + ((ex && ex.description) || r.exceptionDetails.text));
  }
  return r.result.value;
}

async function where(tabId) {
  const t = await chrome.tabs.get(tabId);
  return { url: t.url, title: t.title };
}

async function run(cmd) {
  if (cmd.kind === "look") return look(cmd);
  const g = await grants();
  const entry = Object.entries(g).find(([, v]) => v.session === cmd.session);
  if (!entry) throw new Error("no tab is allowed for this session any more");
  const tabId = Number(entry[0]);
  const a = cmd.args || {};
  switch (cmd.act) {
    case "shot": return shot(tabId);
    case "read": return { ...(await where(tabId)), ...(await inPage(tabId, { func: tabRead, args: [TEXT_MAX, ELEMENTS_MAX] })) };
    case "click": return click(tabId, a);
    case "type": return type(tabId, a);
    case "key": return key(tabId, a);
    case "navigate": return navigate(tabId, a);
    case "scroll": return scroll(tabId, a);
    default: throw new Error(`\`${cmd.act}\` is not a tab act`);
  }
}

async function viewport(tabId) {
  const m = await cdp(tabId, "Page.getLayoutMetrics");
  const v = m.cssVisualViewport || m.visualViewport;
  return { vw: v.clientWidth, vh: v.clientHeight };
}

// A picture in CSS pixels, so its coordinates are the ones click takes.
async function shot(tabId) {
  const shotData = await cdp(tabId, "Page.captureScreenshot", { format: "png" });
  const { vw, vh } = await viewport(tabId);
  let png = shotData.data;
  const bmp = await createImageBitmap(await (await fetch("data:image/png;base64," + png)).blob());
  if (Math.abs(bmp.width - vw) > 1) {
    const c = new OffscreenCanvas(Math.round(vw), Math.round(vh));
    c.getContext("2d").drawImage(bmp, 0, 0, c.width, c.height);
    png = await deps.blobToB64(await c.convertToBlob({ type: "image/png" }));
  }
  return { ...(await where(tabId)), png };
}

// Injected: the page's visible text and the interactive elements in view, numbered in DOM order.
function tabRead(textMax, elementsMax) {
  const SEL = "a[href], button, input:not([type=hidden]), select, textarea, summary, [role=button], [role=link], " +
    "[role=checkbox], [role=tab], [role=menuitem], [role=option], [contenteditable=''], [contenteditable=true], [onclick]";
  const vis = (el) => { const r = el.getBoundingClientRect(); return r.width >= 1 && r.height >= 1 && (!el.checkVisibility || el.checkVisibility()); };
  const inView = (el) => { const r = el.getBoundingClientRect(); return r.bottom > 0 && r.right > 0 && r.top < innerHeight && r.left < innerWidth; };
  const all = Array.from(document.querySelectorAll(SEL)).filter((el) => vis(el) && !el.closest("[data-letibot-ui]"));
  const shown = all.filter(inView);
  const label = (el) => (el.getAttribute("aria-label") || el.innerText || el.value || el.placeholder || el.title || el.name || el.alt || "")
    .replace(/\s+/g, " ").trim().slice(0, 80);
  const unique = (s) => { try { return document.querySelectorAll(s).length === 1; } catch (_) { return false; } };
  const path = (el) => {
    const parts = [];
    for (let e = el; e && e.nodeType === 1 && e !== document.documentElement; e = e.parentElement) {
      if (e.id && unique("#" + CSS.escape(e.id))) { parts.unshift("#" + CSS.escape(e.id)); break; }
      const tag = e.tagName.toLowerCase();
      const same = e.parentElement ? Array.from(e.parentElement.children).filter((c) => c.tagName === e.tagName) : [];
      parts.unshift(same.length > 1 ? `${tag}:nth-of-type(${same.indexOf(e) + 1})` : tag);
    }
    return parts.join(" > ");
  };
  const elements = shown.slice(0, elementsMax).map((el, i) => {
    const r = el.getBoundingClientRect();
    return {
      n: i + 1, tag: el.tagName.toLowerCase(), kind: el.getAttribute("type") || el.getAttribute("role") || "",
      label: label(el), selector: path(el), x: r.x, y: r.y, w: r.width, h: r.height,
      disabled: Boolean(el.disabled || el.getAttribute("aria-disabled") === "true"),
    };
  });
  const text = (document.body ? document.body.innerText : "").replace(/\n{3,}/g, "\n\n").slice(0, textMax);
  return {
    markdown: text, elements, total: all.length,
    scroll_y: scrollY, height: document.documentElement.scrollHeight, vw: innerWidth, vh: innerHeight,
  };
}

// Injected: find element n (as tabRead numbers them) or a selector, scroll it to the middle,
// refuse a disabled one, and return its centre. `focus` also focuses it and selects its text.
function tabTarget(n, selector, focus) {
  const SEL = "a[href], button, input:not([type=hidden]), select, textarea, summary, [role=button], [role=link], " +
    "[role=checkbox], [role=tab], [role=menuitem], [role=option], [contenteditable=''], [contenteditable=true], [onclick]";
  const vis = (el) => { const r = el.getBoundingClientRect(); return r.width >= 1 && r.height >= 1 && (!el.checkVisibility || el.checkVisibility()); };
  const inView = (el) => { const r = el.getBoundingClientRect(); return r.bottom > 0 && r.right > 0 && r.top < innerHeight && r.left < innerWidth; };
  let el = null;
  if (selector) {
    try { el = document.querySelector(selector); } catch (e) {
      return { error: `${JSON.stringify(selector)} is not valid CSS (${e.message})` +
        (/contains/i.test(selector) ? "; `:contains(...)` is jQuery, not CSS. Use `n` from read." : ". Use `n` from read.") };
    }
    if (!el) return { error: `nothing matches ${JSON.stringify(selector)}; read lists what is in view` };
  } else {
    const shown = Array.from(document.querySelectorAll(SEL)).filter((e) => vis(e) && inView(e) && !e.closest("[data-letibot-ui]"));
    el = shown[n - 1];
    if (!el) return { error: `no element ${n}: read lists ${shown.length} in view, and the numbers change when the page scrolls` };
  }
  if (el.disabled || el.getAttribute("aria-disabled") === "true" || el.closest("[disabled], [aria-disabled=true]")) {
    return { error: "that element is DISABLED, so acting on it does nothing. The form is not satisfied yet: read what it still needs rather than retrying." };
  }
  el.scrollIntoView({ block: "center", inline: "center", behavior: "instant" });
  if (focus) {
    el.focus();
    if (typeof el.select === "function") el.select();
    else if (el.isContentEditable) { const r = document.createRange(); r.selectNodeContents(el); const s = getSelection(); s.removeAllRanges(); s.addRange(r); }
  }
  const r = el.getBoundingClientRect();
  const label = (el.getAttribute("aria-label") || el.innerText || el.value || el.placeholder || el.tagName).replace(/\s+/g, " ").trim().slice(0, 80);
  return { x: r.x + r.width / 2, y: r.y + r.height / 2, label, tag: el.tagName.toLowerCase() };
}

// Injected: what the page shows now, to tell a click that did something from one that did not.
function tabSnapshot() {
  return { url: location.href, text: (document.body ? document.body.innerText : "").replace(/\s+/g, " ").trim() };
}

// Injected: what the focused field holds, to check a typed value survived the framework.
function tabFocused() {
  const el = document.activeElement;
  if (!el || el === document.body) return { value: null, tag: "" };
  const value = "value" in el ? String(el.value) : el.isContentEditable ? el.innerText : null;
  return { value, tag: el.tagName.toLowerCase() + (el.id ? "#" + el.id : el.name ? `[name=${el.name}]` : "") };
}

async function target(tabId, a, focus) {
  const t = await inPage(tabId, { func: tabTarget, args: [Number(a.n) || 0, a.selector || "", focus] });
  if (t.error) throw new Error(t.error);
  return t;
}

// Run `act`, then say whether the page changed and whether a new tab opened from this one.
async function judged(tabId, act) {
  const before = await inPage(tabId, { func: tabSnapshot }).catch(() => null);
  let opened = null;
  const onCreated = (t) => { if (t.openerTabId === tabId) opened = t.pendingUrl || t.url || "(not known yet)"; };
  chrome.tabs.onCreated.addListener(onCreated);
  let out;
  try {
    out = await act();
    await sleep(SETTLE_MS);
  } finally {
    chrome.tabs.onCreated.removeListener(onCreated);
  }
  const after = await inPage(tabId, { func: tabSnapshot }).catch(() => null);
  const here = await where(tabId);
  let verdict = "unchanged", fresh = "";
  if (!before || !after || before.url !== here.url) verdict = "navigated";
  else if (before.text !== after.text) {
    verdict = "changed";
    const was = new Set(before.text.split(" "));
    fresh = after.text.split(" ").filter((w) => w && !was.has(w)).slice(0, 60).join(" ");
  }
  return { ...out, ...here, verdict, fresh, ...(opened ? { opened } : {}) };
}

async function click(tabId, a) {
  let x, y, label = "";
  if (a.n || a.selector) ({ x, y, label } = await target(tabId, a, false));
  else if (Number.isFinite(a.x) && Number.isFinite(a.y)) ({ x, y } = a);
  else throw new Error("click needs `n` or `selector` from read, or x and y");
  return judged(tabId, async () => {
    await cdp(tabId, "Input.dispatchMouseEvent", { type: "mouseMoved", x, y });
    await cdp(tabId, "Input.dispatchMouseEvent", { type: "mousePressed", x, y, button: "left", buttons: 1, clickCount: 1 });
    await cdp(tabId, "Input.dispatchMouseEvent", { type: "mouseReleased", x, y, button: "left", buttons: 0, clickCount: 1 });
    return { x, y, label };
  });
}

// Real input through CDP, so React and Vue see their own events; then the value is read back
// after a re-render, because a field the page rewrites must be reported, not assumed.
async function type(tabId, a) {
  const text = String(a.text || "");
  if (!text) throw new Error("type needs `text`");
  let into = "";
  if (a.n || a.selector) into = (await target(tabId, a, true)).tag;
  await cdp(tabId, "Input.insertText", { text });
  await sleep(150);
  const f = await inPage(tabId, { func: tabFocused });
  if (f.value !== null && !f.value.includes(text)) {
    throw new Error(`NOT TYPED: the field did not keep the text - it reads ${JSON.stringify(f.value.slice(0, 80))}. ` +
      "The page controls it. Do not retry the same way: read the form for what it wants first.");
  }
  return { ...(await where(tabId)), typed: text.length, into: into || f.tag };
}

const KEYS = {
  Enter: [13, "Enter", "\r"], Tab: [9, "Tab"], Escape: [27, "Escape"], Esc: [27, "Escape"],
  Backspace: [8, "Backspace"], Delete: [46, "Delete"], Space: [32, "Space", " "],
  ArrowUp: [38, "ArrowUp"], ArrowDown: [40, "ArrowDown"], ArrowLeft: [37, "ArrowLeft"], ArrowRight: [39, "ArrowRight"],
  Up: [38, "ArrowUp"], Down: [40, "ArrowDown"], Left: [37, "ArrowLeft"], Right: [39, "ArrowRight"],
  Home: [36, "Home"], End: [35, "End"], PageUp: [33, "PageUp"], PageDown: [34, "PageDown"],
};
const MODS = { alt: 1, ctrl: 2, control: 2, meta: 4, cmd: 4, shift: 8 };

function keyEvent(spec) {
  const parts = String(spec || "").replace(/^<|>$/g, "").split("+");
  const name = parts.pop();
  let modifiers = 0;
  for (const m of parts) {
    const bit = MODS[m.toLowerCase()];
    if (!bit) throw new Error(`\`${m}\` is not a modifier: Shift, Ctrl, Alt, Meta`);
    modifiers |= bit;
  }
  const k = KEYS[name];
  if (k) {
    const [vk, code, text] = k;
    return { key: code === "Space" ? " " : code, code, windowsVirtualKeyCode: vk, modifiers, text: modifiers & 6 ? undefined : text };
  }
  if (name && name.length === 1) {
    const up = name.toUpperCase();
    const code = /[A-Z]/.test(up) ? "Key" + up : /[0-9]/.test(name) ? "Digit" + name : "";
    return { key: name, code, windowsVirtualKeyCode: up.charCodeAt(0), modifiers, text: modifiers & 6 ? undefined : name };
  }
  throw new Error(`\`${spec}\` is not a key: Enter, Tab, Escape, Backspace, Delete, Space, arrows, Home, End, PageUp, PageDown, or one character, with Shift+ Ctrl+ Alt+ Meta+`);
}

async function key(tabId, a) {
  const ev = keyEvent(a.keys);
  return judged(tabId, async () => {
    await cdp(tabId, "Input.dispatchKeyEvent", { type: ev.text ? "keyDown" : "rawKeyDown", ...ev });
    await cdp(tabId, "Input.dispatchKeyEvent", { type: "keyUp", ...ev, text: undefined });
    return { keys: a.keys };
  });
}

async function navigate(tabId, a) {
  const url = String(a.url || "").trim();
  if (!/^https?:\/\//i.test(url)) throw new Error("navigate takes an http:// or https:// url");
  const r = await cdp(tabId, "Page.navigate", { url });
  if (r.errorText) throw new Error(`the page did not load: ${r.errorText}`);
  const end = Date.now() + LOAD_MS;
  let loaded = false;
  while (Date.now() < end) {
    await sleep(250);
    const s = await cdp(tabId, "Runtime.evaluate", { expression: "document.readyState", returnByValue: true }).catch(() => null);
    if (s && s.result && s.result.value === "complete") { loaded = true; break; }
  }
  return { ...(await where(tabId)), loaded };
}

async function scroll(tabId, a) {
  const { vw, vh } = await viewport(tabId);
  const dy = Number.isFinite(a.dy) ? a.dy : Math.round(vh * 0.8);
  await cdp(tabId, "Input.dispatchMouseEvent", { type: "mouseWheel", x: vw / 2, y: vh / 2, deltaX: 0, deltaY: dy });
  await sleep(300);
  const s = await inPage(tabId, { func: tabRead, args: [0, 0] });
  return { ...(await where(tabId)), scroll_y: s.scroll_y, height: s.height, vh: s.vh };
}

// ---------------------------------------------------------------- looking, without the debugger

const NO_ACCESS = "Chrome no longer lets the extension see this tab: the permission an ask gives " +
  "lasts until the tab navigates. The operator asks again from the page to share it.";

async function look(cmd) {
  const tabId = Number(cmd.tab);
  if (!looks.has(tabId + ":" + cmd.session)) throw new Error("this tab is not shared with this session any more");
  if (cmd.act === "shot") return lookShot(tabId);
  if (cmd.act === "read") return { ...(await where(tabId)), ...(await isolated(tabId, { func: tabRead, args: [TEXT_MAX, ELEMENTS_MAX] })) };
  throw new Error("this tab is shared for looking only, because you were asked about it; the operator allows driving from the popup");
}

// The same fixed functions as driving, in the extension's isolated world.
async function isolated(tabId, { func, args = [] }) {
  let r;
  try {
    [r] = await chrome.scripting.executeScript({ target: { tabId }, world: "ISOLATED", func, args });
  } catch (_) {
    throw new Error(NO_ACCESS);
  }
  if (!r) throw new Error("the page step returned nothing");
  return r.result;
}

// Injected: the viewport in CSS pixels, to scale the capture to the ones read reports.
function lookViewport() {
  return { vw: innerWidth, vh: innerHeight };
}

// captureVisibleTab sees only what the window shows, so a tab behind another is said, not shot.
async function lookShot(tabId) {
  const tab = await chrome.tabs.get(tabId);
  if (!tab.active) {
    throw new Error("the tab is not the active one in its window, so it cannot be captured: " +
      "the operator switches to it, or use `read`, which works on a background tab");
  }
  const { vw, vh } = await isolated(tabId, { func: lookViewport });
  let shotUrl;
  try {
    shotUrl = await deps.withCardHidden(tab, () => chrome.tabs.captureVisibleTab(tab.windowId, { format: "png" }));
  } catch (e) {
    throw new Error(NO_ACCESS + " (" + (e.message || e) + ")");
  }
  const bmp = await createImageBitmap(await (await fetch(shotUrl)).blob());
  let png = shotUrl.slice(shotUrl.indexOf(",") + 1);
  if (Math.abs(bmp.width - vw) > 1) {
    const c = new OffscreenCanvas(Math.round(vw), Math.round(vh));
    c.getContext("2d").drawImage(bmp, 0, 0, c.width, c.height);
    png = await deps.blobToB64(await c.convertToBlob({ type: "image/png" }));
  }
  return { ...(await where(tabId)), png };
}
