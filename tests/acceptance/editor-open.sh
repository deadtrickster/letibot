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

spec "ctrl-] comes back, and with text typed ctrl-e is end-of-line"
press C-]
wait_for "ctrl-] back to the editor"
type_text "abc"
press C-a
press C-e
type_text "!"
wait_for "› abc!"
expect "ctrl-] back to the editor"

done_spec
