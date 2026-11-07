// The popup: which session answers on this site, and the two gestures that need no right-click.

const $ = (id) => document.getElementById(id);
const esc = (s) => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

async function activeTab() {
  const [t] = await chrome.tabs.query({ active: true, currentWindow: true });
  return t;
}

async function load() {
  const tab = await activeTab();
  let host = "";
  try { host = new URL(tab.url).host; } catch (_) { /* not a web page */ }
  $("site").textContent = host || "this tab";
  chrome.runtime.sendMessage({ type: "lb:sessions", url: tab && tab.url }, (r) => {
    if (!r || r.error) {
      $("dot").className = "dot bad";
      $("msg").textContent = (r && r.error) || "No answer from the extension.";
      $("session").innerHTML = "<option value=''>none</option>";
      return;
    }
    $("dot").className = "dot ok";
    const list = r.sessions || [];
    const known = list.some((s) => s.id === r.pick);
    $("session").innerHTML = (known ? "" : "<option value=''>choose a session…</option>") + list.map((s) =>
      `<option value="${esc(s.id)}"${s.id === r.pick ? " selected" : ""}>${esc(s.name)} · ${esc(s.project)} · ${esc(s.state)}</option>`).join("");
    if (!list.length) $("msg").textContent = "No live top-level session.";
  });
  $("session").addEventListener("change", (e) => {
    chrome.runtime.sendMessage({ type: "lb:pick", session: e.target.value, url: tab && tab.url });
  });
}

$("page").addEventListener("click", () => { chrome.runtime.sendMessage({ type: "lb:popup", what: "page" }); window.close(); });
$("shot").addEventListener("click", () => { chrome.runtime.sendMessage({ type: "lb:popup", what: "shot" }); window.close(); });
load();
