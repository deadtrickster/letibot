# Host config - the keyboard and the perf policy, in the platform's own words

`host_config` owns *this box's configuration*: **keyboard and input** and **perf policy**, in a
vocabulary that names the platform instead of pretending there is one, with the privilege
stated and **every change written into the [host changelog](host-changelog.md)**. It is seated
on `leticode` beside `host_changelog`, because the setting and its record are one idea: a seat
that can write down why the governor is `performance` but cannot read what the governor *is*
has the half that arrives a month too late.

## The vocabulary is per platform

"Perf policy" on a Mac and on a Linux box are not the same knob, so they are not the same
name. A setting this platform has not got is refused by name, with what this platform has
instead - a refusal that teaches, never a translation.

| what it is | macOS | Linux |
|---|---|---|
| repeat rate | `key-repeat` - `defaults read -g KeyRepeat`, written `defaults write -g KeyRepeat -int` | `key-repeat-rate` - `xset q`, written `xset r rate <delay> <rate>` |
| delay before the repeat | `key-repeat-delay` - `defaults read -g InitialKeyRepeat`, written `defaults write -g InitialKeyRepeat -int` | `key-repeat-delay` - `xset q`, written `xset r rate <delay> <rate>` |
| perf policy | `perf-policy` - `pmset -g`, written `pmset -a lowpowermode <0\|1>` | `cpu-governor` - `/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor`, written `cpupower frequency-set -g <word>` |

**The units are the platform's own and are printed with every value**, because they are not
interchangeable: macOS counts in 1/60 s (`KeyRepeat: 2` is about 30 ms), X11 counts hertz and
milliseconds, `pmset` takes `0`/`1`, and a governor is a word.

Three things fall out of the platforms and are not tidied away:

- **X11's auto-repeat is one knob with two numbers.** `xset r rate <delay> <rate>` sets both, so
  setting either one carries the other's current value - a `set` that wrote only its half would
  zero the other.
- **The governor's value is checked against the machine** before anything runs:
  `/sys/.../scaling_available_governors` is read, and a word it does not list is refused with
  the list it does.
- **`key-repeat` may be not set** on macOS - `defaults read` answers that with exit 1 and *does
  not exist* on stderr, which is a fact (`before: ""`), not a failure. Reported as `(not set)`;
  there is nothing to record as a `before` until one is written.

Wayland is not in this slice: with no X display the Linux read refuses and says so, naming
Hyprland's `hyprctl getoption input:repeat_rate` as the spelling that would be next.

## Privilege is stated, not assumed

| setting | root? |
|---|---|
| `key-repeat`, `key-repeat-delay` (macOS) | no - `defaults write -g` is this user's domain |
| `key-repeat-delay`, `key-repeat-rate` (Linux) | no - the X server's authority is the display's |
| `perf-policy` (macOS) | **yes** - `pmset` |
| `cpu-governor` (Linux) | **yes** - `cpupower frequency-set` |

A daemon has **no terminal**, so a bare `sudo` is not a prompt, it is a hang. The box's answer
is `letibot-askpass`: `sudo -A` with `SUDO_ASKPASS` naming the helper, and the password
travelling head -> daemon -> the helper's stdout -> sudo, and nowhere else. It is not logged,
not stored, not in the transcript, and never seen by the model.

**A helper on `PATH` is not a channel.** The helper reads no password from anywhere: it
connects to the daemon that owns the session and asks the person in front of a head. So it
needs `LETIBOT_SOCKET` and `LETIBOT_SESSION` too, and neither is in the daemon's own
environment - the launcher passes `--socket` as an argv and the session id is a field. The
harness hands the tool the session's **channel** when it builds the registry, and the tool
puts all four pairs into the command's environment itself:

```
SUDO_ASKPASS     the helper `sudo` runs
LETIBOT_SOCKET   the daemon's socket - what the helper connects to
LETIBOT_SESSION  the session the helper attaches as, so the card reaches the right person
LETIBOT_COMMAND  the command being elevated, so the card says what the password is for
```

A command runs as root only when one of these holds:

- this process is already root -> it runs as it is, no helper and no question;
- a helper is found - `$SUDO_ASKPASS` naming a file, else `letibot-askpass` beside the daemon
  binary (where `harnessd`'s `sudo::install` puts it), else on `PATH` - **and** the channel is
  an address: a non-empty session id, and a socket that is really there;
- otherwise -> **refuse**, before anything runs, naming which one is missing: *this session is
  not root and no way to ask for a password ... A daemon has no terminal, so a bare `sudo` here
  would wait for a prompt nobody can see. Start the daemon under `letibot` (which installs the
  helper), or run the command yourself in a shell you are sitting at.* A helper with no session
  to ask on is the same shape: it attaches on `LETIBOT_SOCKET` and `LETIBOT_SESSION`, and with
  neither it prints *not inside a letibot session* and exits 1 while sudo reports that no
  password was given.

What is *not* claimed: that a password will be typed. The helper waits two minutes for a head
and answers `1` if nobody comes, and sudo's own refusal is then the honest outcome. What the
tool guarantees is that the question can be **asked**, or that the refusal says why not.

## Every change goes into the record

`set` is six acts, and the order is the design:

1. **The checks, before a single command runs.** `setting`, `value`, `why` and `words` are
   required; the value is checked against what the setting takes (`Platform::check`, called
   here and again by `apply`); a setting this platform has not got refuses by name; and a
   session with no changelog behind it refuses here too, since that is a fact about the
   registry rather than about the box.
2. **The `before` is a read.** A read that fails is a refusal, not an empty `before`: a governor
   that could not be read is not a governor that was not set, and a revert would have nothing
   to put back.
3. **The record's door, proved open.** `changelog::writable` creates the config dir and opens
   the file for a line. This comes *after* the read on purpose: proving the door first would
   create an empty `host-changelog.jsonl` for a change that never happened, which reads, a month
   later, as a changelog somebody emptied.
4. **The apply.** Every refusal from here on leaves the box as it was and says so.
5. **The `after` is a read too**, not the value that was asked for, and a command that exits 0
   without moving the setting is reported as such. **A read-back that fails is recorded as
   `after: "unread"`** - the ask is not a measurement - and no setting here can hold that word,
   so it cannot be mistaken for a value.
6. **The record** - the changelog's own `record` verb, so one function writes a line and the
   `before`, `after`, `why`, `words`, host and session are the changelog's own fields.

The reply says what ran, both values, whether the box took it, and the line's number:

```
applied on linux: `cpu-governor` `powersave` → `performance`
  ran: sudo -A cpupower frequency-set -g performance
  the box now reads back `performance`
recorded #4: `cpu-governor` `powersave` → `performance` on lab2x1; `revert 4` is what undoes it
```

The one failure that is not a refusal: **the change was applied and the record did not land.**
The reply says so, prints the exact `host_changelog record` call that would put the line in
the file, and a retried `set` is idempotent - it reads the value, applies the same value and
records it.

## Undoing a change is the changelog's verb

`host_config` has no `revert`. `host_changelog revert N` marks the change undone and **hands
back the recorded `before`**; putting the value back is a `set`, which is a new change,
recorded - the same rule the changelog's own doc states (*a value that has to move again is a
new change to record*). The round trip is a test:
`a_revert_round_trips_through_the_changelogs_own_verb`.
