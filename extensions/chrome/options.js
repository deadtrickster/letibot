// Options: the bridge token and port, kept in chrome.storage.local, never in page reach.

const $ = (id) => document.getElementById(id);

chrome.storage.local.get(["token", "port"]).then((s) => {
  $("token").value = s.token || "";
  $("port").value = s.port || "";
});

$("save").addEventListener("click", async () => {
  const port = Number($("port").value) || 8790;
  await chrome.storage.local.set({ token: $("token").value.trim(), port });
  $("msg").textContent = "Saved. Testing…";
  chrome.runtime.sendMessage({ type: "lb:sessions", url: "" }, (r) => {
    if (!r || r.error) { $("msg").textContent = "Saved, but the bridge refused: " + ((r && r.error) || "no answer"); return; }
    $("msg").textContent = `Connected. ${r.sessions.length} live session(s).`;
  });
});
