<!-- abstract: MEASURED 2026-10-27 on lab2x1: `placement::tests::a_record_of_a_socket_that_is_gone_is_not_an_answer` is red on the tree WITHOUT any change — the fixture's premise ("a rebind is a new inode") is false on ext4, which gave 69773837 twice for `bind, close, unlink, bind`; the record's identity check therefore reads a stale `host:lab2x1` as the live daemon's. -->

# `a_record_of_a_socket_that_is_gone_is_not_an_answer` is red on ext4, and it is not your change

MEASURED 2026-10-27 on lab2x1 (`wt-pg`), while running `cargo test --offline -p letibot-sessionlog`
for an unrelated package: **182 passed, this one failed** — and it fails identically with the only
change I had made to that crate stashed, so it is the tree's, not a package's.

```
thread 'a_record_of_a_socket_that_is_gone_is_not_an_answer' panicked at crates/sessionlog/src/placement.rs:171:9:
assertion `left == right` failed: the record names the socket that is gone, so nothing is known about this one
  left: Some("host:lab2x1")
 right: None
```

## The cause, measured

The fixture removes the old socket and binds a new one at the same path, and asserts that
`placement::read` answers `None` because the record names the socket `(dev, ino)` that is gone:

```python
bind(p) → st_ino 69773837;  close(listener); unlink(p);  bind(p) → st_ino 69773837
```

**ext4 hands the same inode back** when the file is closed before it is unlinked, so
`socket_id(socket) == recorded` and the stale spelling is read as the live daemon's — which is the
exact defect the identity check exists to prevent, arriving through the check itself.

## What to do about it

Either make the ids differ on purpose (`stat` the first inode, and make the test skip-with-a-
sentence if the second answered the same — `apparatus::present`-shaped, per AGENTS.md), or give the
record a stronger identity than `(dev, ino)`. **Do not "fix" it by asserting `Some(...)`**: the
refusal is the property; the fixture is the thing that cannot make its premise true here.

(This is the same shape as `the-macs-su-has-no-dash-c.md`: a test whose fixture is a claim about a
platform, red on a platform where the claim is false.)
