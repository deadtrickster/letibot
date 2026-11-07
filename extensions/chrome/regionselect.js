// Region selector, from oracle's chrome-capture regionselect.js. Drag a rectangle, optionally type
// a question, and the rect goes to the background worker, which crops a captureVisibleTab shot.
// The UI is torn down before the capture so it never lands in the pixels.
(() => {
  if (window.__letibotRegion) return;
  window.__letibotRegion = true;

  const host = document.createElement("div");
  host.setAttribute("data-letibot-ui", "region");
  host.style.cssText = "all:initial;position:fixed;inset:0;z-index:2147483647;cursor:crosshair;";
  const root = host.attachShadow({ mode: "open" });
  root.innerHTML = `
    <style>
      .dim { position:fixed; inset:0; background:rgba(0,0,0,.35); }
      .sel { position:fixed; border:1.5px solid #12e0d8; background:rgba(18,224,216,.08);
        box-shadow:0 0 0 9999px rgba(0,0,0,.35); display:none; }
      .hint { position:fixed; top:10px; left:50%; transform:translateX(-50%); background:#111;
        color:#fff; font:12px sans-serif; padding:6px 12px; border-radius:20px; opacity:.92; }
      .bar { position:fixed; display:none; gap:6px; align-items:center; background:#1e1e1e; cursor:default;
        border:1px solid #444; border-radius:8px; padding:6px; box-shadow:0 8px 30px rgba(0,0,0,.4); }
      .bar input { width:220px; max-width:45vw; border:1px solid #555; background:#111; color:#eee;
        border-radius:6px; padding:5px 7px; font:13px sans-serif; cursor:text; }
      .bar button { border:0; border-radius:6px; padding:5px 9px; font:13px sans-serif; font-weight:600; cursor:pointer; }
      .go { background:#128a86; color:#fff; } .no { background:#333; color:#ccc; }
    </style>
    <div class="dim"></div>
    <div class="sel"></div>
    <div class="hint">Drag to select a region · Esc to cancel</div>
    <div class="bar">
      <input type="text" placeholder="Ask about this region… (blank = describe it)">
      <button class="go">Next</button><button class="no">Cancel</button>
    </div>`;
  document.documentElement.appendChild(host);

  const $ = (s) => root.querySelector(s);
  const dim = $(".dim"), sel = $(".sel"), hint = $(".hint"), bar = $(".bar"), input = $("input");
  let sx, sy, drawing = false, rect = null, box = null;

  function teardown() {
    host.remove();
    window.__letibotRegion = false;
    window.removeEventListener("keydown", onKey, true);
  }
  function onKey(e) { if (e.key === "Escape") { e.preventDefault(); teardown(); } }
  window.addEventListener("keydown", onKey, true);

  // The logical rect from the drag, not getBoundingClientRect: that includes the border.
  function place(x, y) {
    const l = Math.min(x, sx), t = Math.min(y, sy), w = Math.abs(x - sx), h = Math.abs(y - sy);
    box = { x: l, y: t, w, h };
    sel.style.left = l + "px"; sel.style.top = t + "px"; sel.style.width = w + "px"; sel.style.height = h + "px";
  }

  // Measured, then kept inside the viewport: flips above the cursor when there is no room below.
  const MARGIN = 8;
  function placeBar(cx, cy) {
    bar.style.visibility = "hidden";
    bar.style.display = "flex";
    const bw = bar.offsetWidth, bh = bar.offsetHeight;
    const left = Math.max(MARGIN, Math.min(cx - bw / 2, innerWidth - bw - MARGIN));
    let top = cy + 16;
    if (top + bh > innerHeight - MARGIN) top = cy - bh - 16;
    bar.style.left = Math.round(left) + "px";
    bar.style.top = Math.round(Math.max(MARGIN, top)) + "px";
    bar.style.visibility = "visible";
  }

  // The listener is on the shadow host, so e.target is the host: composedPath sees the real node.
  const inBar = (e) => e.composedPath().some((n) => n instanceof Element && n.classList.contains("bar"));

  host.addEventListener("mousedown", (e) => {
    if (inBar(e)) return;
    drawing = true; sx = e.clientX; sy = e.clientY;
    dim.style.display = "none"; bar.style.display = "none"; sel.style.display = "block";
    place(e.clientX, e.clientY); e.preventDefault();
  });
  host.addEventListener("mousemove", (e) => { if (drawing) place(e.clientX, e.clientY); });
  host.addEventListener("mouseup", (e) => {
    if (inBar(e) || !drawing) return;
    drawing = false;
    if (!box || box.w < 8 || box.h < 8) { sel.style.display = "none"; dim.style.display = "block"; return; }
    rect = { ...box };
    hint.style.display = "none";
    placeBar(e.clientX, e.clientY);
    input.focus();
  });

  $(".no").addEventListener("click", teardown);
  function send() {
    const prompt = input.value.trim(), dpr = window.devicePixelRatio || 1;
    teardown();
    setTimeout(() => { try { chrome.runtime.sendMessage({ type: "lb:region", rect, dpr, prompt, viewport: { x: 0, y: 0, w: innerWidth, h: innerHeight } }); } catch (_) { /* worker gone */ } }, 60);
  }
  $(".go").addEventListener("click", send);
  input.addEventListener("keydown", (e) => { e.stopPropagation(); if (e.key === "Enter") send(); });
})();
