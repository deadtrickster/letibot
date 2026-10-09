#!/usr/bin/env bash
# **ctrl-e: the editor on an empty prompt, on any file** — with nothing in the conversation to
# open it on, and end-of-line again the moment there is text in the prompt.
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"

for i in $(seq 1 30); do echo "line $i"; done > "$WORK/notes.txt"
start --replay "$FIXTURES/edit.jsonl"

spec "the conversation is up, and the prompt is empty"
wait_for "Line eleven is fixed."
settle

spec "ctrl-e opens rano, asking for a file"
press C-e
wait_for "Open:"
expect "ctrl-] to the composer"

spec "a path and Enter open it"
type_text "notes.txt"
press Enter
wait_for "line 17"
expect_not "Open:"

spec "ctrl-] comes back, and the editor stays up behind the prompt"
press C-]
wait_for "ctrl-] back to the editor"
expect "line 17"

spec "ctrl-e on the empty prompt puts the editor away, and brings it back as it was"
press C-e
wait_gone "line 17"
expect "Line eleven is fixed."
press C-e
wait_for "line 17"
press C-]
wait_for "ctrl-] back to the editor"

spec "with text typed ctrl-e is end-of-line"
type_text "abc"
press C-a
press C-e
type_text "!"
wait_for "› abc!"
expect "ctrl-] back to the editor"

done_spec
