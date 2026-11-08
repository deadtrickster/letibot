// letibot card, glued to the selection. Adapted from oracle's chrome-capture overlay.js.
// Injected on demand and idempotent. Shadow DOM keeps the page's CSS out; no network here,
// the background worker holds the token and talks to the bridge.
(() => {
  if (window.__letibotCard) return;
  window.__letibotCard = true;

  let host = null, root = null;
  let streaming = false, acc = "", answerEl = null, askId = "", watchdog = null, sessionName = "";

  const esc = (s) => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

  // Escape first, then re-introduce a small set of formatting: the reply is never raw HTML.
  function md(text) {
    let s = esc(text);
    s = s.replace(/```(\w*)\n([\s\S]*?)```/g, (_m, _l, code) => "<pre><code>" + code + "</code></pre>");
    s = s.replace(/`([^`]+)`/g, "<code>$1</code>");
    s = s.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
    s = s.replace(/^\s*[-*]\s+(.*)$/gm, "<li>$1</li>").replace(/(<li>[\s\S]*?<\/li>)/g, "<ul>$1</ul>");
    s = s.replace(/\n{2,}/g, "</p><p>").replace(/\n/g, "<br>");
    return "<p>" + s + "</p>";
  }

  // Fixed to the viewport, not the page: a conversation must not scroll away with the article.
  // It opens under the selection when there is one, clamped so the whole card stays on screen.
  function place() {
    const sel = window.getSelection();
    let top = 80, left = 40;
    if (sel && sel.rangeCount) {
      const r = sel.getRangeAt(0).getBoundingClientRect();
      if (r.width || r.height) { top = r.bottom + 8; left = r.left; }
    }
    const w = document.documentElement.clientWidth, h = window.innerHeight;
    host.style.top = Math.max(8, Math.min(top, h * 0.3 - 8)) + "px";
    host.style.left = Math.max(8, Math.min(left, w - 440)) + "px";
  }

  function makeDraggable(handle) {
    let sx, sy, ox, oy, drag = false;
    handle.addEventListener("mousedown", (e) => {
      if (e.composedPath().some((n) => n instanceof Element && (n.tagName === "SELECT" || n.tagName === "BUTTON"))) return;
      drag = true; sx = e.clientX; sy = e.clientY;
      ox = parseFloat(host.style.left); oy = parseFloat(host.style.top);
      e.preventDefault();
    });
    window.addEventListener("mousemove", (e) => {
      if (!drag) return;
      host.style.left = ox + (e.clientX - sx) + "px";
      host.style.top = oy + (e.clientY - sy) + "px";
    });
    window.addEventListener("mouseup", () => { drag = false; });
  }

  const CSS = `
    .card { width:420px; max-width:92vw; max-height:70vh; display:flex; flex-direction:column;
      font:13px/1.5 -apple-system,Segoe UI,Roboto,sans-serif; background:#fff; color:#1a1a1a;
      border:1px solid #d0d0d0; border-radius:10px; box-shadow:0 8px 30px rgba(0,0,0,.25); }
    @media (prefers-color-scheme: dark) {
      .card { background:#1e1e1e; color:#e6e6e6; border-color:#3a3a3a; }
      pre, code { background:#2a2a2a !important; } a { color:#6ab0ff; }
    }
    .bar { display:flex; align-items:center; gap:8px; padding:8px 12px; cursor:move;
      border-bottom:1px solid rgba(128,128,128,.25); }
    .bar b { font-size:12px; opacity:.85; }
    .bar select { flex:1; min-width:0; font:inherit; font-size:12px; padding:3px 4px; border-radius:6px;
      border:1px solid rgba(128,128,128,.4); background:transparent; color:inherit; }
    .x { cursor:pointer; border:0; background:transparent; color:inherit; font-size:16px; opacity:.6; }
    .x:hover { opacity:1; }
    .log { padding:8px 12px; overflow:auto; flex:1; }
    .thumb { max-width:100%; border-radius:6px; margin-bottom:8px; display:block; }
    .quote { border-left:3px solid rgba(128,128,128,.4); padding-left:8px; opacity:.75; margin:0 0 8px;
      white-space:pre-wrap; max-height:6em; overflow:hidden; }
    .q { font-weight:600; margin:10px 0 4px; white-space:pre-wrap; }
    .a p { margin:0 0 8px; } .a ul { margin:6px 0; padding-left:18px; }
    pre { background:#f4f4f4; padding:8px; border-radius:6px; overflow:auto; }
    code { background:#f0f0f0; padding:1px 4px; border-radius:4px; font-family:ui-monospace,monospace; }
    pre code { background:transparent; padding:0; }
    .note { font-size:11px; opacity:.7; margin:4px 0; }
    .err { color:#c0392b; }
    .ask { display:flex; gap:6px; padding:8px 12px; border-top:1px solid rgba(128,128,128,.25); }
    .ask textarea { flex:1; resize:vertical; min-height:2.4em; font:inherit; padding:5px 7px; border-radius:7px;
      border:1px solid rgba(128,128,128,.4); background:transparent; color:inherit; }
    .ask button { border:0; border-radius:7px; padding:6px 12px; font:inherit; font-weight:600;
      background:#128a86; color:#fff; cursor:pointer; }
    .ask button:disabled { opacity:.5; cursor:default; }
    .caret { display:inline-block; width:6px; height:1.05em; vertical-align:text-bottom;
      background:currentColor; opacity:.55; animation:bl 1s steps(2) infinite; margin-left:1px; }
    @keyframes bl { 50% { opacity:0; } }`;

  function open(msg) {
    if (!host || !document.documentElement.contains(host)) {
      host = document.createElement("div");
      host.setAttribute("data-letibot-ui", "card");
      host.style.cssText = "all:initial;position:fixed;z-index:2147483647;";
      root = host.attachShadow({ mode: "open" });
      document.documentElement.appendChild(host);
    }
    place();
    root.innerHTML = `<style>${CSS}</style>
      <div class="card">
        <div class="bar"><b>letibot</b><select title="The session that answers"></select>
          <button class="x" title="Close">×</button></div>
        <div class="log"></div>
        <div class="ask"><textarea placeholder="Ask… (Enter sends, Shift+Enter is a new line)"></textarea>
          <button>Ask</button></div>
      </div>`;
    const log = root.querySelector(".log");
    if (msg.thumb) log.insertAdjacentHTML("beforeend", `<img class="thumb" src="${esc(msg.thumb)}">`);
    if (msg.selection) log.insertAdjacentHTML("beforeend", `<div class="quote">${esc(msg.selection.slice(0, 600))}</div>`);
    root.querySelector(".x").addEventListener("click", () => { host.remove(); host = null; });
    makeDraggable(root.querySelector(".bar"));
    const ta = root.querySelector("textarea");
    ta.value = msg.question || "";
    ta.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); submit(); }
    });
    root.querySelector(".ask button").addEventListener("click", submit);
    root.querySelector("select").addEventListener("change", (e) => {
      chrome.runtime.sendMessage({ type: "lb:pick", session: e.target.value, url: location.href });
    });
    loadSessions();
    ta.focus();
  }

  function note(text, cls = "") {
    const log = root.querySelector(".log");
    log.insertAdjacentHTML("beforeend", `<div class="note ${cls}">${esc(text)}</div>`);
    log.scrollTop = log.scrollHeight;
  }

  function loadSessions() {
    const sel = root.querySelector("select");
    sel.innerHTML = "<option value=''>loading sessions…</option>";
    chrome.runtime.sendMessage({ type: "lb:sessions", url: location.href }, (r) => {
      if (!root) return;
      if (!r || r.error) { sel.innerHTML = "<option value=''>no sessions</option>"; note((r && r.error) || "No answer from the extension.", "err"); return; }
      const list = r.sessions || [];
      if (!list.length) { sel.innerHTML = "<option value=''>no live sessions</option>"; note("No live top-level session. Start one with `letibot` in a project.", "err"); return; }
      // No default: the last pick for this site, else the operator chooses.
      const known = list.some((s) => s.id === r.pick);
      sel.innerHTML = (known ? "" : "<option value=''>choose a session…</option>") + list.map((s) =>
        `<option value="${esc(s.id)}"${s.id === r.pick ? " selected" : ""}>${esc(s.name)} · ${esc(s.project)} · ${esc(s.state)}</option>`).join("");
    });
  }

  function submit() {
    if (streaming) return;
    const session = root.querySelector("select").value;
    const ta = root.querySelector("textarea");
    const question = ta.value.trim();
    if (!session) { note("Choose a session first.", "err"); return; }
    if (!question) return;
    const log = root.querySelector(".log");
    log.insertAdjacentHTML("beforeend", `<div class="q">${esc(question)}</div><div class="a"><span class="caret"></span></div>`);
    answerEl = log.lastElementChild;
    acc = "";
    streaming = true;
    sessionName = root.querySelector("select").selectedOptions[0].textContent;
    root.querySelector(".ask button").disabled = true;
    ta.value = "";
    askId = Math.random().toString(36).slice(2) + Date.now().toString(36);
    chrome.runtime.sendMessage({ type: "lb:ask", id: askId, session, question });
    watchdog = setInterval(checkAlive, 10000);
  }

  // MV3 can kill the worker mid-stream, and then no event ever says so. Ask it: a restarted
  // worker does not know this ask, so the card says the reply stopped instead of looking done.
  function checkAlive() {
    if (!streaming) return;
    const id = askId;
    try {
      chrome.runtime.sendMessage({ type: "lb:alive", id }, (r) => {
        if (!streaming || id !== askId || (r && r.alive)) return;
        note(`The reply stopped here: the extension's worker was restarted. The session still has the question and keeps answering - read the rest in letibot (${sessionName}), or ask again.`, "err");
        finish();
      });
    } catch (_) {
      note("The reply stopped here: the extension was reloaded. Read the rest in letibot, or ask again.", "err");
      finish();
    }
  }

  function paint() {
    if (!answerEl) return;
    answerEl.innerHTML = md(acc) + (streaming ? '<span class="caret"></span>' : "");
    const log = root.querySelector(".log");
    log.scrollTop = log.scrollHeight;
  }

  function finish() {
    streaming = false;
    if (watchdog) { clearInterval(watchdog); watchdog = null; }
    if (root) root.querySelector(".ask button").disabled = false;
    paint();
  }

  chrome.runtime.onMessage.addListener((msg) => {
    if (msg.type === "lb:open") { open(msg); return; }
    if (msg.type !== "lb:event" || !root) return;
    const { event, data } = msg.ev || {};
    if (event === "accepted") note(`sent to ${data.name || data.session}`);
    else if (event === "status") note(data.text || "");
    else if (event === "delta") { acc += data.text || ""; paint(); }
    else if (event === "done") { if (data.how && data.how !== "ended") note(`turn ${data.how}`); finish(); }
    else if (event === "error") { note(data.error || "error", "err"); finish(); }
  });
})();
