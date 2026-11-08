// The popup: which session answers on this site, the two gestures that need no right-click, and
// the switch that lets the picked session drive this tab.

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
    drive(tab);
  });
  $("session").addEventListener("change", (e) => {
    chrome.runtime.sendMessage({ type: "lb:pick", session: e.target.value, url: tab && tab.url });
    drive(tab);
  });
  $("drive").addEventListener("change", (e) => toggle(tab, e.target.checked));
}

function picked() {
  const o = $("session").selectedOptions[0];
  return o && o.value ? { session: o.value, name: o.textContent.split(" · ")[0] } : null;
}

// The switch shows the grant this tab has, or offers the picked session one.
function drive(tab, note = "") {
  if (!tab) return;
  chrome.runtime.sendMessage({ type: "lb:tabstate", tabId: tab.id }, (r) => {
    const grant = r && r.grant;
    const p = picked();
    $("drive").checked = Boolean(grant);
    if (grant) {
      $("drivelabel").textContent = `${grant.name} may drive this tab`;
      $("drivemsg").textContent = "Chrome's debugging bar stays up while it may. Untick, cancel the bar or close the tab to stop it.";
      $("drive").disabled = false;
    } else {
      $("drivelabel").textContent = p ? `Let ${p.name} drive this tab` : "Pick a session to let it drive this tab";
      $("drivemsg").textContent = p ? "It can click, type, navigate, read and screenshot this tab only." : "";
      $("drive").disabled = !p || !/^https?:/i.test(tab.url || "");
    }
    if (note) $("drivemsg").textContent = note;
  });
}

function toggle(tab, on) {
  const p = picked();
  const msg = on ? { type: "lb:allow", tabId: tab.id, session: p && p.session, name: p && p.name } : { type: "lb:revoke", tabId: tab.id };
  if (on && !p) { $("drive").checked = false; return; }
  $("drive").disabled = true;
  chrome.runtime.sendMessage(msg, (r) => drive(tab, (r && r.error) || (r ? "" : "No answer from the extension.")));
}

$("page").addEventListener("click", () => { chrome.runtime.sendMessage({ type: "lb:popup", what: "page" }); window.close(); });
$("shot").addEventListener("click", () => { chrome.runtime.sendMessage({ type: "lb:popup", what: "shot" }); window.close(); });
load();
