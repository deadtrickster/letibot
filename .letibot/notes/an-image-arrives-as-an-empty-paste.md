<!-- abstract: Pasting a picture: a terminal's paste key on an image sends an EMPTY bracketed paste, ctrl-v was already the payload window (so the clipboard goes first unless a window is open), and a `User` row's first `Text` part is load-bearing in two places. -->

# Pasting a picture: the empty paste, the taken key, and the first text part

A terminal's paste key on an image sends `Key::Paste("")` - a bracketed paste with no text in it,
because there are no bytes to paste - so the door is `pasted.trim().is_empty()`, not a key the
terminal never sends. That is why `App::paste_arrived` treats an empty paste as "ask the clipboard".

`Key::CtrlV` was already the payload window, so the rule is: the clipboard first, unless a window is
open. A press that closes a window must close it, and with no picture on the clipboard nothing is
said and the key is the window it always was.

A `User` row's FIRST `Text` part is load-bearing in two places. `App::retire_pending` spends a row's
lines against the queued echo, front to back, so the operator's words must be the leading text of the
row or the echo says `queued` for ever. And the row is drawn from the text parts while the daemon
appends an attachment's inlined file content as a later `Text` part, so `user_text` returns the first
`Text` part only - drawing them all put a 30 KB CSV in the conversation.

`letibot_transcript::media::sniff_mime` checks magic numbers and then falls back to the extension, so
a `.png` that is really text is an image at BOTH ends (the head's chip and the daemon's part).
Consistent is what matters; do not "fix" that on one side only.

A test build's clipboard is `FakeClipboard(Ok(None))` by construction (`default_clipboard()` under
`cfg(test)`), so a test that wants a picture calls
`App::set_clipboard(FakeClipboard(Ok(Some(png))))`.
