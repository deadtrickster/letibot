// letibot browser extension, background service worker. Adapted from oracle's chrome-capture.
// All bridge traffic happens here: an https page cannot fetch http://127.0.0.1 (mixed content),
// and the token must never be handed to page script.

import { pumpSSE } from "./sse.js";
import { setupTabs, allow, revoke, stateOf, lookStart, lookEnd, poll } from "./tab.js";

const DEFAULT_PORT = 8790;

// ---------------------------------------------------------------- settings and the bridge

async function settings() {
  const s = await chrome.storage.local.get(["token", "port"]);
  return { token: (s.token || "").trim(), port: Number(s.port) || DEFAULT_PORT };
}

// One door to the bridge. A refusal comes back as `{error}` JSON, so the card can say it.
async function bridge(path, init = {}) {
  const { token, port } = await settings();
  if (!token) {
    throw new Error("No token. Run `letibot browser`, then paste the token it printed into this extension's options.");
  }
  let r;
  try {
    r = await fetch(`http://127.0.0.1:${port}${path}`, {
      ...init,
      headers: { ...(init.headers || {}), Authorization: "Bearer " + token },
    });
  } catch (_) {
    throw new Error(`The bridge is not answering on 127.0.0.1:${port}. Start it with \`letibot browser\`.`);
  }
  if (!r.ok) {
    let why = `bridge said ${r.status}`;
    try { why = (await r.json()).error || why; } catch (_) { /* not JSON */ }
    throw new Error(why);
  }
  return r;
}

async function sessions() {
  return (await bridge("/sessions")).json();
}

// The last session picked per site. A preselection only: the operator always sees the picker.
async function pickFor(host) {
  const { picks } = await chrome.storage.local.get("picks");
  return (picks || {})[host] || "";
}

async function rememberPick(host, session) {
  if (!host || !session) return;
  const { picks } = await chrome.storage.local.get("picks");
  await chrome.storage.local.set({ picks: { ...(picks || {}), [host]: session } });
}

function hostOf(url) {
  try { return new URL(url).host; } catch (_) { return ""; }
}

// ---------------------------------------------------------------- menus

// Preset questions: they only fill the card's box, so the operator can still edit them.
const PRESETS = {
  explain: "Explain this in plain words, in the context of the page it is on.",
  factcheck: "Fact-check this. Verdict first - true, false or unclear - then the evidence, with sources.",
  pageShot: "Explain this page. The picture is what I see of it.",
};

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({ id: "ask-sel", title: "Ask letibot about this selection", contexts: ["selection"] });
  chrome.contextMenus.create({ id: "explain", title: "Explain this with letibot", contexts: ["selection"] });
  chrome.contextMenus.create({ id: "factcheck", title: "Fact-check this with letibot", contexts: ["selection"] });
  chrome.contextMenus.create({ id: "ask-page", title: "Ask letibot about this page", contexts: ["page"] });
  chrome.contextMenus.create({ id: "page-shot", title: "Explain this page with letibot (with a picture)", contexts: ["page"] });
  chrome.contextMenus.create({ id: "shot", title: "Screenshot a region -> ask letibot", contexts: ["page", "image", "selection"] });
  chrome.contextMenus.create({ id: "img", title: "Explain this image", contexts: ["image"] });
});

chrome.contextMenus.onClicked.addListener((info, tab) => {
  if (info.menuItemId === "ask-sel") askSelection(tab, info.selectionText || "");
  else if (info.menuItemId === "explain") askSelection(tab, info.selectionText || "", PRESETS.explain);
  else if (info.menuItemId === "factcheck") askSelection(tab, info.selectionText || "", PRESETS.factcheck);
  else if (info.menuItemId === "ask-page") openCard(tab, { mode: "page" });
  else if (info.menuItemId === "page-shot") pageWithPicture(tab);
  else if (info.menuItemId === "shot") screenshotRegion(tab);
  else if (info.menuItemId === "img") explainImage(info.srcUrl, tab);
});

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  const tab = sender.tab;
  if (msg.type === "lb:sessions") {
    const host = hostOf(msg.url || (tab && tab.url) || "");
    Promise.all([sessions(), pickFor(host)])
      .then(([s, pick]) => sendResponse({ ...s, pick }))
      .catch((e) => sendResponse({ error: e.message }));
    return true;
  }
  if (msg.type === "lb:pick") {
    rememberPick(hostOf(msg.url || (tab && tab.url) || ""), msg.session).then(() => sendResponse({ ok: true }));
    return true;
  }
  if (msg.type === "lb:ask" && tab) { ask(tab, msg.id, msg.session, msg.question); return false; }
  // A worker restarted mid-stream has an empty set: the card reads that as a reply that stopped.
  if (msg.type === "lb:alive") { sendResponse({ alive: live.has(msg.id) }); return false; }
  if (msg.type === "lb:region" && tab) { regionShot(msg, tab); return false; }
  if (msg.type === "lb:tabstate") {
    stateOf(msg.tabId).then((grant) => sendResponse({ grant })).catch((e) => sendResponse({ error: e.message }));
    return true;
  }
  if (msg.type === "lb:allow") {
    allow(msg.tabId, msg.session, msg.name).then(() => sendResponse({ ok: true }))
      .catch((e) => sendResponse({ error: e.message || String(e) }));
    return true;
  }
  if (msg.type === "lb:revoke") {
    revoke(msg.tabId).then(() => sendResponse({ ok: true })).catch((e) => sendResponse({ error: e.message }));
    return true;
  }
  if (msg.type === "lb:popup") {
    chrome.tabs.query({ active: true, currentWindow: true }, ([t]) => {
      if (!t) return;
      if (msg.what === "page") openCard(t, { mode: "page" });
      else if (msg.what === "shot") screenshotRegion(t);
    });
    sendResponse({ ok: true });
    return true;
  }
  return false;
});

// ---------------------------------------------------------------- the card

function scriptableTab(tab) {
  return tab && tab.id >= 0 && tab.url && /^https?:/i.test(tab.url);
}

// What an ask carries besides the question. Kept in storage.session, because the worker can be
// torn down while the operator is still typing and an in-memory map would lose the screenshot.
async function setPending(tabId, pending) {
  await chrome.storage.session.set({ ["pending:" + tabId]: pending });
}

async function getPending(tabId) {
  return (await chrome.storage.session.get("pending:" + tabId))["pending:" + tabId] || {};
}

async function openCard(tab, { mode, selection = "", image = "", imageContext = {}, thumb = "", question = "" }) {
  if (!scriptableTab(tab)) return;
  await setPending(tab.id, { mode, selection, image, imageContext });
  try {
    await chrome.scripting.executeScript({ target: { tabId: tab.id }, files: ["overlay.js"] });
    await chrome.tabs.sendMessage(tab.id, { type: "lb:open", mode, selection, thumb, question });
  } catch (_) { /* a page that refuses injection (the web store, chrome://) has no card */ }
}

async function askSelection(tab, fallback, question = "") {
  // Read the selection now: clicking into the card's question box clears it.
  let selection = fallback;
  try {
    const [r] = await chrome.scripting.executeScript({
      target: { tabId: tab.id },
      func: () => String(window.getSelection() || ""),
    });
    if (r && r.result && r.result.trim()) selection = r.result;
  } catch (_) { /* the menu's own copy will do */ }
  openCard(tab, { mode: "selection", selection: selection.trim(), question });
}

// ---------------------------------------------------------------- page -> Markdown

// Injected, so it must be self-contained: nothing outside its body exists in the page.
// A light readability pass: the article or main element when there is one, chrome skipped.
function pageMarkdown() {
  const SKIP = new Set(["SCRIPT", "STYLE", "NOSCRIPT", "TEMPLATE", "IFRAME", "SVG", "CANVAS",
    "NAV", "ASIDE", "FORM", "BUTTON", "INPUT", "SELECT", "TEXTAREA", "DIALOG"]);
  const BLOCK = new Set(["P", "DIV", "SECTION", "ARTICLE", "MAIN", "HEADER", "FOOTER", "FIGURE",
    "FIGCAPTION", "DL", "DT", "DD", "DETAILS", "SUMMARY", "ADDRESS"]);
  const root = document.querySelector("article") || document.querySelector("main, [role=main]") || document.body;
  const chromeTags = root === document.body ? new Set(["HEADER", "FOOTER"]) : new Set();
  const cell = (s) => s.trim().replace(/\s*\n\s*/g, " ").replace(/\|/g, "\\|");

  function conv(node, pre) {
    if (node.nodeType === Node.TEXT_NODE) return pre ? node.nodeValue : node.nodeValue.replace(/\s+/g, " ");
    if (node.nodeType !== Node.ELEMENT_NODE) return "";
    const el = node, tag = el.tagName;
    if (SKIP.has(tag) || chromeTags.has(tag) || el.hasAttribute("data-letibot-ui")) return "";
    if (el.hidden || el.getAttribute("aria-hidden") === "true") return "";
    if (el.checkVisibility && !el.checkVisibility()) return "";
    const kids = () => Array.from(el.childNodes).map((c) => conv(c, pre)).join("");
    const level = /^H([1-6])$/.exec(tag);
    if (level) return "\n\n" + "#".repeat(Number(level[1])) + " " + kids().trim() + "\n\n";
    switch (tag) {
      case "BR": return "\n";
      case "HR": return "\n\n---\n\n";
      case "PRE": return "\n\n```\n" + el.innerText.replace(/\n+$/, "") + "\n```\n\n";
      case "CODE": return pre ? kids() : "`" + el.textContent + "`";
      case "STRONG": case "B": { const k = kids(); return k.trim() ? "**" + k.trim() + "**" : k; }
      case "EM": case "I": { const k = kids(); return k.trim() ? "_" + k.trim() + "_" : k; }
      case "BLOCKQUOTE": return "\n\n" + kids().trim().split("\n").map((l) => "> " + l).join("\n") + "\n\n";
      case "A": {
        const text = kids().trim();
        const href = el.getAttribute("href") || "";
        if (!text) return "";
        if (!href || href.startsWith("#") || /^javascript:/i.test(href)) return text;
        return `[${text}](${el.href})`;
      }
      case "IMG": {
        const alt = (el.alt || "").trim();
        const src = el.currentSrc || el.src || "";
        if (!alt) return "";
        return src.startsWith("data:") ? `[image: ${alt}]` : `![${alt}](${src})`;
      }
      case "UL": case "OL": {
        const items = Array.from(el.children).filter((li) => li.tagName === "LI");
        return "\n\n" + items.map((li, i) => (tag === "OL" ? `${i + 1}. ` : "- ") +
          conv(li, pre).trim().replace(/\n+/g, "\n   ")).join("\n") + "\n\n";
      }
      case "TABLE": {
        const rows = Array.from(el.rows).map((r) => "| " + Array.from(r.cells).map((c) => cell(conv(c, pre))).join(" | ") + " |");
        if (!rows.length) return "";
        const width = el.rows[0].cells.length;
        rows.splice(1, 0, "|" + " --- |".repeat(width));
        return "\n\n" + rows.join("\n") + "\n\n";
      }
      default:
        return BLOCK.has(tag) ? "\n\n" + kids().trim() + "\n\n" : kids();
    }
  }

  const markdown = conv(root, false).replace(/[ \t]+\n/g, "\n").replace(/\n{3,}/g, "\n\n").trim();
  return { url: location.href, title: document.title, markdown };
}

// ---------------------------------------------------------------- ask

// An MV3 worker is torn down after ~30s idle and a long fetch does not reliably count, so touch
// an extension API while a reply streams.
let keepalive = null, keepaliveDepth = 0;
function keepaliveStart() {
  keepaliveDepth++;
  if (!keepalive) keepalive = setInterval(() => chrome.runtime.getPlatformInfo().catch(() => {}), 20000);
}
function keepaliveStop() {
  keepaliveDepth = Math.max(0, keepaliveDepth - 1);
  if (keepaliveDepth === 0 && keepalive) { clearInterval(keepalive); keepalive = null; }
}

function utf8Len(s) { return new TextEncoder().encode(s || "").length; }

// The site's own agent brief, `https://<host>/AGENTS.md`, cached per host with misses, since
// nearly every site lacks one. activeTab grants the tab's origin, which covers this fetch.
const AGENTS_TTL_MS = 24 * 3600 * 1000;
const AGENTS_TIMEOUT_MS = 2500;

async function agentsMd(url) {
  let origin;
  try { origin = new URL(url).origin; } catch (_) { return ""; }
  if (!/^https?:/.test(origin)) return "";
  const key = "agents:" + origin;
  const hit = (await chrome.storage.local.get(key))[key];
  if (hit && Date.now() - hit.at < AGENTS_TTL_MS) return hit.text;
  let text = "";
  try {
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(), AGENTS_TIMEOUT_MS);
    const r = await fetch(origin + "/AGENTS.md", { signal: ctl.signal, credentials: "omit", redirect: "error" });
    clearTimeout(timer);
    const type = r.headers.get("content-type") || "";
    // A single-page app answers every path with its HTML shell and a 200; that is not a brief.
    if (r.ok && !/html/i.test(type)) {
      const body = await r.text();
      if (!body.trimStart().startsWith("<")) text = body;
    }
  } catch (_) { text = ""; }
  await chrome.storage.local.set({ [key]: { at: Date.now(), text } });
  return text;
}

// The live asks of THIS worker. Module scope on purpose: a restarted worker starts empty.
const live = new Set();

async function ask(tab, id, session, question) {
  const send = (ev) => chrome.tabs.sendMessage(tab.id, { type: "lb:event", ev }).catch(() => {});
  question = (question || "").trim();
  if (!session) { send({ event: "error", data: { error: "Pick a session first." } }); return; }
  if (!question) { send({ event: "error", data: { error: "Type a question." } }); return; }
  keepaliveStart();
  live.add(id);
  // The asked session may look at this tab while it answers; the bridge ends that at turn end.
  lookStart(tab.id, session);
  try {
    const pending = await getPending(tab.id);
    let page = { url: tab.url, title: tab.title, markdown: "" };
    try {
      const [r] = await chrome.scripting.executeScript({ target: { tabId: tab.id }, func: pageMarkdown });
      if (r && r.result) page = r.result;
    } catch (_) { /* injection refused: the question still goes, without the page */ }

    // Over the bridge's limit the page is left out and BOTH sides are told, never cut silently.
    const { limits } = await sessions();
    let markdown = page.markdown || "", markdownDropped = 0;
    if (limits && utf8Len(markdown) > limits.markdown) {
      markdownDropped = utf8Len(markdown);
      markdown = "";
      send({ event: "status", data: { text: `Page text not sent: ${Math.round(markdownDropped / 1024)} KiB is over the bridge's ${Math.round(limits.markdown / 1024)} KiB limit.` } });
    }
    let agents = await agentsMd(tab.url), agentsDropped = 0;
    if (limits && utf8Len(agents) > limits.agents_md) {
      agentsDropped = utf8Len(agents);
      agents = "";
      send({ event: "status", data: { text: `The site's AGENTS.md not sent: ${Math.round(agentsDropped / 1024)} KiB is over the bridge's ${Math.round(limits.agents_md / 1024)} KiB limit.` } });
    }
    const ctx = pending.image ? (pending.imageContext || {}) : {};
    const body = {
      session, question, url: page.url || tab.url, title: page.title || tab.title,
      selection: pending.selection || "", markdown, markdown_dropped: markdownDropped,
      agents_md: agents, agents_md_dropped: agentsDropped,
      image: pending.image || "",
      image_source: ctx.source || "", image_context: ctx.text || "",
      image_alt: ctx.alt || "", image_title: ctx.title || "", image_caption: ctx.caption || "",
      tab: tab.id,
    };
    await rememberPick(hostOf(tab.url), session);
    const r = await bridge("/ask", {
      method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body),
    });
    poll();
    // The image rides the first question only; a follow-up in the card is text.
    await setPending(tab.id, { ...pending, image: "" });
    let terminal = false;
    const unreadable = await pumpSSE(r.body, (ev) => {
      if (ev.event === "done" || ev.event === "error") terminal = true;
      send(ev);
    });
    if (unreadable) {
      send({ event: "status", data: { text: `The stream sent ${unreadable} bytes I could not read; the reply above may be missing part of the text.` } });
    }
    if (!terminal) {
      send({ event: "error", data: { error: "The stream ended early (the bridge stopped?). The session still has the question; read the reply there." } });
    }
  } catch (e) {
    send({ event: "error", data: { error: e.message || String(e) } });
  } finally {
    lookEnd(tab.id, session);
    live.delete(id);
    keepaliveStop();
  }
}

// ---------------------------------------------------------------- screenshots and images

async function screenshotRegion(tab) {
  if (!scriptableTab(tab)) return;
  try {
    await chrome.scripting.executeScript({ target: { tabId: tab.id }, files: ["regionselect.js"] });
  } catch (_) { /* injection refused */ }
}

async function blobToB64(blob) {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  let s = "";
  for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
  return btoa(s);
}

// tabs.sendMessage drops an oversized message in silence, so the card gets a small JPEG.
async function thumbnail(dataUrl, maxW = 600) {
  try {
    const bmp = await createImageBitmap(await (await fetch(dataUrl)).blob());
    const scale = Math.min(1, maxW / bmp.width);
    const c = new OffscreenCanvas(Math.max(1, Math.round(bmp.width * scale)), Math.max(1, Math.round(bmp.height * scale)));
    c.getContext("2d").drawImage(bmp, 0, 0, c.width, c.height);
    return "data:image/jpeg;base64," + await blobToB64(await c.convertToBlob({ type: "image/jpeg", quality: 0.72 }));
  } catch (_) {
    return "";
  }
}

// Our own card is hidden for the capture, or the model reads its previous answer as the site.
async function withCardHidden(tab, fn) {
  const set = (hide) => chrome.scripting.executeScript({
    target: { tabId: tab.id }, args: [hide],
    func: (h) => {
      for (const el of document.querySelectorAll("[data-letibot-ui]")) el.style.visibility = h ? "hidden" : "";
    },
  }).catch(() => {});
  await set(true);
  await new Promise((r) => setTimeout(r, 90));
  try { return await fn(); } finally { await set(false); }
}

// Injected: the text the DOM renders inside the crop, so the pixels arrive with their words.
function cropText(rect) {
  const out = [];
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  const range = document.createRange();
  let total = 0;
  for (let n = walker.nextNode(); n && total < 20000; n = walker.nextNode()) {
    const t = n.nodeValue.replace(/\s+/g, " ").trim();
    if (!t || (n.parentElement && n.parentElement.closest("[data-letibot-ui]"))) continue;
    range.selectNodeContents(n);
    const b = range.getBoundingClientRect();
    if (!b.width || !b.height) continue;
    if (b.right < rect.x || b.left > rect.x + rect.w || b.bottom < rect.y || b.top > rect.y + rect.h) continue;
    out.push(t);
    total += t.length;
  }
  return out.join(" ");
}

async function regionShot(msg, tab) {
  const { rect, dpr, prompt } = msg;
  // A canvas or a chart has no DOM text inside the crop; then the whole viewport's text goes,
  // labelled as such, because a crop sent with no words gets a confabulated reading.
  let inside = "", source = "region";
  const textIn = async (r) => {
    try {
      const [res] = await chrome.scripting.executeScript({ target: { tabId: tab.id }, args: [r], func: cropText });
      return (res && res.result) || "";
    } catch (_) { return ""; }
  };
  inside = await textIn(rect);
  if (!inside.trim() && msg.viewport) { inside = await textIn(msg.viewport); source = "viewport"; }
  try {
    const shot = await withCardHidden(tab, () => chrome.tabs.captureVisibleTab(tab.windowId, { format: "png" }));
    const bmp = await createImageBitmap(await (await fetch(shot)).blob());
    const sw = Math.max(1, Math.round(rect.w * dpr)), sh = Math.max(1, Math.round(rect.h * dpr));
    const canvas = new OffscreenCanvas(sw, sh);
    canvas.getContext("2d").drawImage(bmp, Math.round(rect.x * dpr), Math.round(rect.y * dpr), sw, sh, 0, 0, sw, sh);
    const image = "data:image/png;base64," + await blobToB64(await canvas.convertToBlob({ type: "image/png" }));
    openCard(tab, {
      mode: "region", image, thumb: await thumbnail(image), question: prompt || "What is in this region?",
      imageContext: { source, text: inside },
    });
  } catch (e) {
    openCard(tab, { mode: "region", question: prompt || "" });
    chrome.tabs.sendMessage(tab.id, { type: "lb:event", ev: { event: "error", data: { error: "Screenshot failed: " + (e.message || e) } } }).catch(() => {});
  }
}

// The page ask plus what the operator sees of it, attached the way a region's picture is.
async function pageWithPicture(tab) {
  if (!scriptableTab(tab)) return;
  try {
    const image = await withCardHidden(tab, () => chrome.tabs.captureVisibleTab(tab.windowId, { format: "png" }));
    openCard(tab, {
      mode: "page", image, thumb: await thumbnail(image), question: PRESETS.pageShot,
      imageContext: { source: "page", text: "" },
    });
  } catch (e) {
    openCard(tab, { mode: "page", question: PRESETS.pageShot });
    chrome.tabs.sendMessage(tab.id, { type: "lb:event", ev: { event: "error", data: { error: "Screenshot failed: " + (e.message || e) } } }).catch(() => {});
  }
}

async function explainImage(srcUrl, tab) {
  if (!scriptableTab(tab) || !srcUrl) return;
  let meta = { alt: "", title: "", caption: "", near: "", dataUrl: "" };
  try {
    const [r] = await chrome.scripting.executeScript({
      target: { tabId: tab.id }, args: [srcUrl],
      func: (src) => {
        const clean = (s) => (s || "").replace(/\s+/g, " ").trim();
        const el = [...document.images].find((i) => i.currentSrc === src || i.src === src);
        if (!el) return { alt: "", title: "", caption: "", near: "", dataUrl: "" };
        const cap = el.closest("figure") && el.closest("figure").querySelector("figcaption");
        const around = el.closest("figure, article, section, div") || el.parentElement;
        const near = clean(around && around.innerText).slice(0, 1500);
        let dataUrl = "";
        try {
          const c = document.createElement("canvas");
          c.width = el.naturalWidth || el.width;
          c.height = el.naturalHeight || el.height;
          if (c.width && c.height) { c.getContext("2d").drawImage(el, 0, 0); dataUrl = c.toDataURL("image/png"); }
        } catch (_) { dataUrl = ""; }
        return { alt: clean(el.alt), title: clean(el.title), caption: clean(cap && cap.textContent), near, dataUrl };
      },
    });
    meta = (r && r.result) || meta;
  } catch (_) { /* injection refused */ }
  let image = meta.dataUrl;
  if (!image) {
    // A cross-origin image taints the canvas; the worker may fetch it only from the tab's origin.
    try {
      const blob = await (await fetch(srcUrl)).blob();
      if (blob.type.startsWith("image/")) image = `data:${blob.type};base64,${await blobToB64(blob)}`;
    } catch (_) { image = ""; }
  }
  await openCard(tab, {
    mode: "image", image, thumb: image ? await thumbnail(image) : "", question: "Explain this image.",
    imageContext: { source: "image", text: meta.near, alt: meta.alt, title: meta.title, caption: meta.caption },
  });
  if (!image) {
    chrome.tabs.sendMessage(tab.id, { type: "lb:event", ev: { event: "status", data: { text: "Could not read that image (another origin). Use Screenshot a region instead, or ask without it." } } }).catch(() => {});
  }
}

// Last, so everything it calls is initialised. Its listeners still register synchronously.
setupTabs({ bridge, keepaliveStart, keepaliveStop, blobToB64, withCardHidden });
