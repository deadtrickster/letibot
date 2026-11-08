# Browser extension

Ask one of your letibot sessions about the page you are reading, or let one drive a tab you
allow. Right-click a selection, a page,
an image or a screenshot region. A card glued to the page shows the session that answers, a
question box and the reply as it streams. The question lands in the session as your prompt, so
the conversation and its tools are the session's own.

```text
extension worker ──HTTP 127.0.0.1:8790 + bearer token──▶ letibot browser ──unix socket──▶ the session's daemon
```

## Install

1. Start the bridge: `letibot browser`. On the first run it makes `~/.config/letibot/browser.token`
   (mode 0600) and prints the token once. Later runs say where the file is.
2. In Chrome open `chrome://extensions`, turn on Developer mode, click Load unpacked and pick
   `extensions/chrome` in this repository.
3. Open the extension's options (Details, Extension options), paste the token, leave the port
   at 8790 unless you changed it, and press Save and test. It answers with how many live
   sessions it sees.

The `key` in `manifest.json` pins the extension id to `kbaoealgdkldcgaojgdehmdaaekkmffd`, and the
bridge answers that origin only. A copy loaded under another id needs `extension_id` below.

Config, in `~/.config/letibot/leticode.toml` only (a project file is writable by its sessions and
cannot choose who may prompt them):

```toml
[browser]
port = 8790
extension_id = "kbaoealgdkldcgaojgdehmdaaekkmffd"
```

`letibot browser --port N` overrides the port for one run.

## Use

Context menu:

- Ask letibot about this selection - the selection, then the page.
- Ask letibot about this page.
- Screenshot a region -> ask letibot - drag a rectangle, type a question or leave it blank.
- Explain this image - the image's own pixels, its `alt`, `title` and `figcaption`.

The popup has the session picker for the current site and the page and region gestures.

The picker lists your live top-level sessions: name, project and state (`idle`, `running`,
`asking`). There is no default. The last pick on a site is preselected the next time, and you can
change it in the card or the popup. A follow-up typed in the card goes to the same session.

Every ask carries the URL, the title, the selection, the page as Markdown (a readability pass in
the extension: the article or main element, chrome skipped) and the site's `/AGENTS.md` when it
has one (cached per origin for a day, misses included). A region carries the DOM text inside the
crop, or the whole viewport's text when the crop has none. An image carries the text around it.

The reply streams until the turn that took the question ends. A question sent while the session
is busy waits for the session to take it, and the card says so. If the extension's worker dies
mid-reply the card says the reply stopped; the session keeps answering and the rest is in
letibot.

## Drive a tab

A session can act on one tab you allow: click, type, press keys, navigate, scroll, read and
screenshot it. Nothing else of the browser.

Allow: open the popup on the tab, pick the session, tick "Let <session> drive this tab". The
extension attaches `chrome.debugger` to that tab, and Chrome shows its "letibot started debugging
this browser" bar for as long as the grant lasts. One tab per session: allowing a second tab
moves the grant. A tab holds one session.

Revoke, any of:

- untick the switch in the popup,
- press Cancel on Chrome's debugging bar,
- close the tab.

The session uses the `screen` tool's `tab` op: `grants`, `shot`, `read`, `click`, `type`, `key`,
`navigate`, `scroll`. A session with no grant is refused with "no tab is allowed for this session -
the operator allows one from the extension popup". If `letibot browser` is not running, the tool
says so.

What the session can do, and only on the allowed tab:

- `shot` - a PNG of the viewport (`Page.captureScreenshot`), scaled to CSS pixels.
- `read` - the title, URL, visible text and a numbered list of the interactive elements in view,
  each with a selector and a box. It runs one fixed script of the extension's in an isolated
  world, so the page's own script cannot see or patch it.
- `click` - an element number from `read`, a CSS selector, or x,y, as a real mouse press
  (`Input.dispatchMouseEvent`). A disabled control is refused before the click. The answer says
  whether the page navigated, changed or stayed the same, and names a tab the click opened.
- `type` - real text input (`Input.insertText`) into the focused field, or into an element it
  focuses first, replacing its text. The field is read back after a re-render and a field that
  did not keep the text is reported as not typed.
- `key` - Enter, Tab, Escape, Backspace, Delete, arrows, Home, End, PageUp, PageDown, Space or
  one character, with Shift, Ctrl, Alt, Meta (`Input.dispatchKeyEvent`).
- `navigate` - http or https only, in the same tab.
- `scroll` - the mouse wheel, by CSS pixels.

What it cannot do: run a script of its own (there is no evaluate act), read cookies or storage,
use the Network domain, or touch another tab. A link that opens a new tab is reported, not
followed. Every command the bridge carries is logged on its stderr with the session, the tab's
URL, the act and the outcome, and each one is a `screen` call in the session's own transcript.

How it runs: the session's daemon writes one JSON line to `<rundir>/browser.sock` (0600, the same
directory as the daemon sockets). The bridge queues it for the extension's worker, which pulls it
with a long poll on `POST /tab/next` (same Host, Origin and token doors), runs it through CDP and
posts the result to `/tab/result`. A command the worker does not take in 10 s, or does not answer
in 40 s, fails with that reason. Each poll carries the extension's grant list, which is the truth:
a restarted bridge learns it again in one round, and the extension detaches a tab whose session
the bridge refuses (not one of your live top-level sessions). A worker that has not polled for a
minute (Chrome closed) has its grants refused until it does.

The socket admits any process of yours, like the daemon sockets: the grant, not the caller, is
what the bridge checks, and a session only ever acts through the fixed acts above.

## Limits

Stated by `GET /sessions` and refused with both sizes when broken, never cut:

| field | limit | from |
|---|---|---|
| question | 16 KiB | |
| selection, image context | 64 KiB | |
| page Markdown | 256 KiB | the daemon's ceiling for inlining an attached text file |
| AGENTS.md | 16 KiB | |
| image | 16 MiB | `grep::FILE_CEILING`, the largest file the tree opens |

The extension checks the page and AGENTS.md against these first. Over the limit it leaves the
block out, says so in the card, and the prompt says `[the page's Markdown was not sent: ...]`.

## Security model

- **Loopback only.** The bridge binds 127.0.0.1 and refuses any other address. The `Host` header
  must be `127.0.0.1:<port>` or `localhost:<port>`, so a DNS name rebound to loopback is refused.
- **One origin.** An `Origin` must be `chrome-extension://<id>`. A request with another origin is
  refused, and a refusal carries no allow-origin header, so a web page cannot read it. A request
  with none passes this door, because Chrome sends none on the extension's own GET, and still
  needs the token: a page cannot add the bearer header without a CORS request, which names its
  origin.
- **A token.** `Authorization: Bearer <token>`, compared in constant time. The file is 0600 and
  the bridge refuses one readable by others. The token lives in the extension's
  `chrome.storage.local`, and only the background worker talks to the bridge: page script never
  sees it.
- **Top-level sessions only.** Asks go to the operator's own conversations, the same "mine"
  group the session list draws. Subagents, packages, the review queue and one-shots are not
  listed and are refused by id. The listing is re-read on every ask.
- **The page is data.** The question is the only operator text. The page, the selection, the
  text beside an image and AGENTS.md are each fenced in the harness's `UNTRUSTED_TEXT` envelope
  (the one `web_fetch` uses), labelled as data and not instruction, with `<<<` spaced out so the
  page cannot close the fence. AGENTS.md is marked written by the site, not instructions and not
  citable. The image's `alt`/`title`/`figcaption` are marked as the author's own description.
- **No decisions.** The bridge's head attaches as `letibot-browser` with `can_decide: false`: it
  prompts and reads, and every permission card goes to your own head.
- **Pictures** are written 0600 into `~/.local/share/letibot/browser/` (0700) for the daemon to
  read, and files older than a day are removed when the bridge starts. A model that takes text
  only gets the harness's `[image ... not sent: <model> takes text only]` placeholder.

## Wire

`GET /sessions` answers `{sessions: [{id, name, project, workspace, state, model}], limits}`.

`POST /ask` takes `{session, question, url, title, selection, markdown, markdown_dropped,
agents_md, agents_md_dropped, image, image_source, image_context, image_alt, image_title,
image_caption}` and answers an SSE stream: `accepted` `{ask, session, name, project}`, `status`
`{text}`, `delta` `{text}`, then `done` `{how: ended|interrupted}` or `error` `{error}`.

`POST /tab/sync` and `POST /tab/next` take `{grants: [{session, tab, url, title}]}` and answer
`{kept, refused: [{session, why}], command}`; `/tab/next` holds up to 20 s for a `command`
`{id, session, tab, act, args}`. `POST /tab/result` takes `{id, ok, result | error}`.

The socket takes `{session, act, args}` and answers `{ok, grant, result}` or `{ok: false, error}`.

The extension's JavaScript is checked by `extensions/chrome/check.sh`, which needs node:
`firecode exec bash check.sh` from that folder.
