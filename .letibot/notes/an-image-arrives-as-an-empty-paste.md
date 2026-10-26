<!-- abstract: MEASURED 2026-10-26 building paste attachments (protocol 50): a terminal's paste key on an image sends an EMPTY bracketed paste, ctrl-v was already the payload-window chord, and a User row's first Text part is load-bearing for the head's queued echo. -->

# Pasting a picture: the empty paste, the taken key, and the first text part

Branch `paste-attachments`, protocol 50. Three facts that cost a decision each and would
have to be rediscovered.

## A terminal's paste key on an image sends `Key::Paste("")`

`Cmd+V` with a picture on the clipboard produces a bracketed paste with **no text in it** —
there are no bytes to paste — so the door is `pasted.trim().is_empty()`, not a key the terminal
never sends. (iTerm2's `OSC 1337 File=` "paste as file" is a different door this head does not
decode.) That is why `App::paste_arrived` treats an empty paste as *ask the clipboard*.

## Ctrl+V was already the payload window

`Key::CtrlV` opened/closes the newest long tool result (`app/keys/chords.rs`, R10/R56). Image
paste had to share it, so the rule is: **the clipboard first, unless a window is open** — a press
that closes a window must close it, and with no picture on the clipboard nothing is said and the
key is the window it always was. The cost of the rule is one `osascript` spawn (~50 ms) per press.

## A `User` row's FIRST `Text` part is load-bearing in two places, not one

- `App::retire_pending` spends a row's LINES against the queued echo, front to back, so the
  operator's words must be the leading text of the row or the echo says `queued` for ever.
- The row is DRAWN from the text parts, and the daemon appends an attachment's inlined file
  content as a later `Text` part — so `user_text` (`tui/src/ui/transcript/user.rs`) now returns
  the first `Text` part only. Drawing them all put a 30 KB CSV in the conversation.

The chips themselves stay in the operator's words (`[image 1 · png · 92 KiB]`), which is what
makes the row show exactly what the composer showed and lets the model read the same label.

## `sniff_mime` falls back to the EXTENSION

`letibot_transcript::media::sniff_mime` checks magic numbers first and then `.png/.jpg/.gif/.webp`
by name. So a `.png` that is really text is an "image" at BOTH ends (the head's chip and the
daemon's part) — consistent, which is what matters, but it means a "picture" whose bytes are junk
still reaches the server as one. Do not "fix" that on one side only.

## A test build's clipboard is the fake, by construction

`app::attachments::default_clipboard()` returns `FakeClipboard(Ok(None))` under `cfg(test)`, so
no existing ctrl-v test could reach the operator's real clipboard when the key grew this meaning.
A test that wants a picture calls `App::set_clipboard(FakeClipboard(Ok(Some(png))))`.
