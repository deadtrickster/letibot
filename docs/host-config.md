# Host config, the first slice — the keyboard and the perf policy, in the platform's own words

The operator's ask, from the room's design: a `host` tool — the seat that owns *this box's
configuration* — starting with **keyboard and input** and **perf policy**, in a vocabulary that
names the platform instead of pretending there is one, with the privilege stated, and **every
change written into the [host changelog](host-changelog.md)**.

The tool is `host_config`, and it is seated on `leticode` beside `host_changelog`: the setting and
its record are one idea, and a seat that can write down why the governor is `performance` but
cannot read what the governor *is* has the half that arrives a month too late.

## The vocabulary is per platform

"Perf policy" on a Mac and on a Linux box are **not the same knob**, so they are not the same
name. A setting this platform has not got is refused by name, with what this platform has instead
— a refusal that teaches, and never a translation.

| what it is | macOS | Linux |
|---|---|---|
| repeat rate | `key-repeat` — `defaults read -g KeyRepeat`, written `defaults write -g KeyRepeat -int` | `key-repeat-rate` — `xset q`, written `xset r rate <delay> <rate>` |
| delay before the repeat | `key-repeat-delay` — `defaults read -g InitialKeyRepeat`, written `defaults write -g InitialKeyRepeat -int` | `key-repeat-delay` — `xset q`, written `xset r rate <delay> <rate>` |
| perf policy | `perf-policy` — `pmset -g`, written `pmset -a lowpowermode <0\|1>` | `cpu-governor` — `/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor`, written `cpupower frequency-set -g <word>` |

**The units are the platform's own and are printed with every value**, because they are not
interchangeable: macOS counts in 1/60 s (`KeyRepeat: 2` is about 30 ms), X11 counts hertz and
milliseconds, `pmset` takes `0`/`1`, and a governor is a word.

Two things fall out of the platforms and are not tidied away:

* **X11's auto-repeat is one knob with two numbers.** `xset r rate <delay> <rate>` sets both, so
  setting either one carries the other's current value — a `set` that wrote only its half would
  zero the other.
* **The governor's value is checked against the machine** before anything runs:
  `/sys/…/scaling_available_governors` is read and a word it does not list is refused with the
  list it does. A word from another kernel is a refusal here, not a half-changed CPU.

macOS's `key-repeat` may legitimately be **not set** — the key was never written and macOS's own
default applies. That is reported as `(not set)` and not as an empty value: there is nothing to
record as a `before` until one is written.

Wayland is not in this slice. On a session with no X display the Linux read refuses and says so,
naming Hyprland's `hyprctl getoption input:repeat_rate` / `input:repeat_delay` as the spelling
that would be next.

## Privilege is stated, not assumed

| setting | root? |
|---|---|
| `key-repeat`, `key-repeat-delay` (macOS) | no — `defaults write -g` is this user's domain |
| `key-repeat-delay`, `key-repeat-rate` (Linux) | no — the X server's authority is the display's |
| `perf-policy` (macOS) | **yes** — `pmset` |
| `cpu-governor` (Linux) | **yes** — `cpupower frequency-set` |

A daemon has **no terminal**, so a bare `sudo` is not a prompt, it is a hang. The box's answer is
`letibot-askpass`: the daemon's `sudo` shim execs `sudo -A` with `SUDO_ASKPASS` pointing at the
helper, and the password travels head → daemon → sudo's stdin and nowhere else. So the tool looks
for a channel before it runs anything:

* this process is already root → run the command as it is;
* `$SUDO_ASKPASS` names a file, or `letibot-askpass` is on `PATH` → `sudo -A <command>` with the
  helper in the command's environment;
* neither → **refuse**, before anything runs:

  > this setting needs root, and this session cannot ask for it — this session is not root and has
  > no way to ask for a password: `$SUDO_ASKPASS` names nothing and `letibot-askpass` is not on
  > `PATH`. A daemon has no terminal, so a bare `sudo` here would wait for a prompt nobody can
  > see. Start the daemon under `letibot` (which installs the helper), or run the command
  > yourself in a shell you are sitting at.

## Every change goes into the record

`set` is three acts, and the order is the design:

1. **The checks, before a single command runs.** `setting`, `value`, `why` and `words` are
   required, the value is checked against what the setting takes, and a platform that has not got
   the setting refuses by name. A change with no `why` is refused *here* — the only moment at
   which refusing costs nothing.
2. **The record's door, proved open.** `changelog::writable` creates the config dir if it is not
   there and opens the file for a line. A file that cannot be written stops the change, because a
   change that lands with no line in the file is exactly the state the changelog exists to end.
3. **The `before` is a read.** The platform is asked what the setting is *now*. A read that fails
   is a refusal, not an empty `before` — a governor that could not be read is not a governor that
   was not set, and a revert would have nothing to put back.
4. **The apply.** Every refusal from here on leaves the box as it was and says so.
5. **The `after` is a read too**, not the value that was asked for. A command that exits 0 and
   does not move the setting is reported as such, and the record carries what the machine says.
6. **The record** — the changelog's own `record` verb, so one function writes a line and the
   `before`, `after`, `why`, `words`, host and session are the changelog's own fields.

The reply says what ran, both values, whether the box took it, and the number of the line:

```
applied on linux: `cpu-governor` `powersave` → `performance`
  ran: sudo -A cpupower frequency-set -g performance
  the box now reads back `performance`

recorded #4: `cpu-governor` `powersave` → `performance` on lab2x1
  why: the box slept through the test window
  words: "make it fast, i don't care about the fans"
  by s-7f3a
  4 change(s) in /home/dead/.config/letibot/host-changelog.jsonl now; `revert 4` is what undoes it …
```

The one failure that is not a refusal: **the change was applied and the record did not land.** The
reply says so in as many words, prints the exact `host_changelog record` call that would put the
line in the file, and a retried `set` is idempotent — it reads the value, applies the same value
and records it.

## Undoing a change is the changelog's verb

`host_config` has no `revert`. `host_changelog revert N` marks the change undone and **hands back
the recorded `before`**; putting the value back is a `set`, which is a new change, recorded — the
same rule the changelog's own doc states (*a value that has to move again is a new change to
record*). The round trip is a test:
`a_revert_round_trips_through_the_changelogs_own_verb`.

## What the next slice should add

* **Wayland** — `hyprctl getoption input:repeat_rate` / `input:repeat_delay` and `hyprctl keyword
  input:repeat_rate`, the same two numbers behind a different door.
* **A `clear` verb.** macOS's `key-repeat` may be *not set*, and putting that back is `defaults
  delete -g KeyRepeat`, which is a third act the two-value model has no word for. The changelog
  already allows `before: ""`; the apply needs a verb that can produce it.
* **More settings, one at a time, with the same two checks**: `vm.swappiness`, a unit's
  `LimitNOFILE`, an `fstab` option, a power profile. Each one is a read, a write and a unit — and
  the list is where a person learns which of them need root.
* **The other machine's settings.** `host_config` reads and writes **this box**, and a placed
  session's tools run on the declared host — so a `host` argument (the changelog already carries
  one) would let a change made on `lab2x1` be recorded with `host: "lab2x1"` and read back there.
  Today the record's `host` is this box's name and that is honest: the tool cannot yet reach
  another machine's `pmset`.
* **A verify step that knows what "took" means** for a setting whose read-back lags (macOS's
  `defaults` reaches a running app at its next start), rather than reporting the mismatch and
  leaving it to the reader.
