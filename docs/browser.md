# Browser extension

Ask one of your letibot sessions about the page you are reading. Right-click a selection, a page,
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
- **One origin.** The `Origin` must be `chrome-extension://<id>`. A request with another origin or
  none is refused, and a refusal carries no allow-origin header, so a web page cannot read it.
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

The extension's JavaScript is checked by `extensions/chrome/check.sh`, which needs node:
`firecode exec bash check.sh` from that folder.
