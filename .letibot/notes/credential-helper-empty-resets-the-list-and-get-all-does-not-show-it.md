<!-- abstract: `-c credential.helper=` really does clear the inherited helpers, so every GitHub push names both words; `git config --get-all` cannot show that it worked, and `git credential fill` prints the real token. -->

# `credential.helper=` clears the effective list, and `--get-all` cannot show that

`-c credential.helper=` really does clear the inherited helpers. Without it an inherited helper
answers FIRST and the named one is never asked: a repo whose `credential.helper` was a stub answering
`username=inherited`, run with only `-c credential.helper='!f() { ... }'`, was answered by
`osxkeychain` with the operator's own account, and the stub never ran. That is the whole reason every
GitHub push carries both words (`crates/harnessd/src/mergequeue/git.rs`):

```sh
git -c credential.helper= -c credential.helper='!gh auth git-credential' push ...
```

The rake: `git config --get-all credential.helper` says the opposite. The empty value appears as a
BLANK LINE in the raw list and the earlier entries are still printed, because `--get-all` prints the
file's values and not the list git's credential machinery ends up with. A reader who checks the
clearing this way concludes it did nothing; `git credential fill`, with the password redacted, is the
measurement that answers the question.

And the trap under it: `credential fill` prints the real token on stdout. Never call it without
`| sed -E 's/^password=.*/password=<redacted>/'`.
