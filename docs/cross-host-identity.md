# Stable session identities across hosts, through migrations

Status: design. TODO row R138. Builds on `docs/answer-waits.md` (R136), `docs/remote-hosts.md`
"Migrating a session" (RH4) and `docs/cross-project-todo.md` (R137).

A session that migrates keeps its name and its open asks, as asker and as target. Then an ask may
cross hosts, and the refusal `AskOtherHost` (`crates/room/src/server.rs:2393`) goes away.

## What happens today

- The room is one host's (`docs/room.md` "The coordinator"). An ask's id is that room's chat `seq`
  (`crates/room/src/chat.rs:464`, `append_ask` `:926`), so the id means nothing on another host.
- `letibot --migrate` moves four things (`crates/letibot/src/migrate.rs:14`) and none is the room.
  The `todo` table travels (`crates/tokencore/src/store/transfer.rs:20`), so A's `Ask` edges arrive
  on the destination holding ids its room never minted.
- The destination daemon is `harnessd --session ID ... --no-queue` (`migrate.rs:1290`) and no
  coordinator is started there, so the migrated session has `NoAsks` (`crates/tools/src/builtins/todo.rs:2057`).
- On the source host the session's connection closes, `gone_since` starts (`server.rs:169`) and
  after 5 minutes the operator is asked to restart a session that is running fine elsewhere.

So a migration today never settles an ask wrongly (the services contract holds), but it turns every
open ask into a false escalation, and A's pass meets an unknown id and raises an operator ask
(`docs/answer-waits.md` "Edge cases"). Evidence from the live store, read-only: no session has
`lives_on` set, no ask edge exists. Nothing in use needs converting.

## Identity

| part | what | stable across a move | source |
|---|---|---|---|
| `id` | `s-<unix nanos>[-n]` | yes, it is THE identity | `mint_session_id`, `crates/sessionlog/src/server.rs:2053` |
| name | the handle, a slug of the title | yes, the title travels in the `session` row | `Member.handle`, `crates/room/src/tree.rs:227` |
| project | room key, hash of the canonical root path | no, the destination tree is `root/<repo>/<short id>` | `project_key`, `tree.rs:70` |
| repo | root commit of the tree | yes, the tree moves as a bundle of the same history | new |
| host | where the live copy is | changes on a move, by definition | `session.lives_on`, `crates/tokencore/src/store.rs:292` |

Decisions:

- The id is the identity and is not rewritten. Rooms already key asks by session id (`AskView.asker`,
  `target`), so asks survive a rename too. No host in the id: it would lie after the first move.
  Collisions between hosts need two mints in one nanosecond, and an import already refuses an id the
  store holds (`transfer.rs:246`).
- The name is an alias resolved where it is typed, as `resolve_handle` does now (`server.rs:1078`):
  this host first, then the fleet directory below. Two live sessions with one name on two hosts are
  ambiguous and refused with both ids, as two in one room are. `name@host` is the explicit spelling,
  and it is a lookup hint, not part of the identity.
- The project key stays per host: it names a room and a socket, and a path is what a room is for.
  Cross-host matching uses the repo id: the first root commit (`git rev-list --max-parents=0 HEAD`,
  oldest), kept on the directory entry. Reason: the bundle keeps history, so the root commit is the
  one fact both trees share.
- The host is the `lives_on` index in the operator's store, which is already the index of record
  (`migrate.rs:27`). Nothing else claims to know where a session lives.

## Topology: the hub is authoritative, hosts never dial out

Rooms stay host-local. Same-host asks stay in the host room, unchanged and with no new latency.
Cross-host state has one writer: the **hub**, the room on the box that holds the `lives_on` index
and `hosts.toml`. The hub holds the fleet directory (id to host, name, repo) and every ask whose two
ends are on different hosts.

The hub dials, hosts never do. Reason: no keys leave the operator's box (`docs/remote-hosts.md`
"No keys leave this box"), so a host cannot open ssh back. The hub reaches each host's room through
the forward `migrate.rs` already builds (`forward`, `migrate.rs:1328`, `ssh -N -L`) and presents that
room's operator token, read over the same ssh (`<socket>.token`, `docs/room.md` "Identity, seats
and the operator"). The connection is a new seat, `relay`, which only the token grants. No new
secret.

Rejected: peer-to-peer rooms (every pair needs keys and a consistency rule), one coordinator for the
fleet (the room's 0600 socket and peer-uid check are host-local by design), and homing an ask at the
target's room (a target's move would re-home the ask and change its id under the asker).

The hub's sweep is the room's 5 s clock. Per host with a live session it reads presence (who holds a
connection, `gone_since`) and pushes the mirror of hub asks touching that host's sessions. A host room
answers `Asks` for its sessions from its own asks plus that mirror, so `crates/harnessd/src/room/asks.rs`
reads one list and does not know which room holds an ask.

## Ask identity

`AskRef { home, seq }`: the room that minted it, by host key (`crates/room/src/lease.rs:315`), and its
seq there. A name, not an address: it never changes, even when the ask moves to the hub.

- `TodoNeed::Ask` and `TodoNeed::Owed` gain `home: Option<String>` (serde default, `None` means this
  host's room, which is every edge written so far).
- `AskView` gains `home` the same way. A host room keys its mirror by `(home, seq)`. The chat `seq`
  of the local copy of the question is display only (`thread`).

## What a migration is

The existing four moves plus a fifth, the asks, run by `migrate.rs` on the hub, which runs every
migration already (host to host is relayed through it, `docs/remote-hosts.md:608`). After
`quiesce_and_shut` (`migrate.rs:717`) and before `export` (`:897`):

1. **Hand off.** Hub to source room: `Handoff { session, to }`. The source returns every open ask
   with the session at either end and marks them `moving`. A moving ask is open, is not escalated,
   and its `gone_since` is not counted. Reason: a migration is not a fault the operator owes.
2. **Journal.** The hub writes `migration(session, from, to, phase, asks_json, ms)` to its store, a
   new table (the next schema version, `Additive`). A re-run of `--migrate` reads it and resumes at the phase.
3. **Store rows.** Export, carry, import, unchanged. Edges keep their `AskRef`s.
4. **Re-home.** Each handed-off ask goes where its ends now are: both on one host, that host's room;
   otherwise the hub. The hub sends `Adopt { asks }` with their `AskRef`s kept. The source drops them
   on the hub's `Release` after the adopt is acknowledged. Two phases, the hub coordinating.
5. **Placement.** `lives_on` and the directory entry are written. The destination daemon is started
   and the hub ensures a coordinator there, the way `room_ensure` does on a local start, so the
   session has a room link.
6. **Clear.** The first `Hello` of the session on the destination clears `moving`.

The question text travels in the ask, so a destination room posts a local copy of the question with
ref `{kind: ask, home, seq}`. Threads do not merge across hosts.

Wire, all additive, `ROOM_PROTOCOL_VERSION` 11 to 12: seat `relay`; frames `Handoff`, `Adopt`,
`Release`, `Presence` (read), `Mirror` (push), `Directory` (push, so `resolve_name` can resolve a
remote name). `Ask`, `Reply`, `Decline`, `Withdraw` and `Retire` are unchanged in shape; a host
room forwards any of them that names a hub ask over the relay. Old coordinators are refused, as a
moving ask read as plain open would escalate.

## Failure cases

| case | rule |
|---|---|
| Migration fails before re-home | The hub sends `Unhandoff`. Asks go back to open at the source, nothing settled. |
| Hub dies mid-migration | `moving` has a deadline, the migration's own bound plus the grace. Past it, the source reverts the asks to open and the normal escalation runs. The journal row lets `--migrate` resume or undo. |
| Hub down, no migration | Same-host asks unaffected. Cross-host asks settle nothing and escalate nothing new: a host room keeps its last mirror and says the hub is unreachable. Room down is not B down (R136). |
| Hub cannot reach a host | Not B gone. After the grace the operator is asked about the host: *cannot reach lab2x1, `arango` waits on `serenedb`*. A distinct sentence, because restarting B is the wrong repair. |
| Two live copies (an interrupted move resumed by hand) | The directory follows `lives_on`. A `Hello` of the session on a host the directory does not place it on is noted, and its `Reply`/`Decline` for hub asks are refused (`NotWhereItLives`). |
| Clock skew between hosts | Each room measures `gone_since` on its own clock. The hub uses only durations a host reports, never compares stamps across hosts. |
| A session deleted on a remote host | `Retire` to its host room, relayed to the hub on the next sweep, settles `Down` there. |
| Name reused on another host after a delete | Asks are by id, so nothing open is captured. A new ask by name resolves to the live one. |
| Destination has no coordinator and the hub cannot start one | The migration stops before the placement step and unhands, so a session never lands with open asks and no room. |

## Package plan

Paths are disjoint. R137's P1 lands first (protocol 11), then R138's.

| pkg | paths | done-tests |
|---|---|---|
| Q1 identity types | `crates/tokencore/src/store.rs` (`TodoNeed` `home`), `crates/sessionlog/src/event.rs`, conversions in `crates/harnessd/src/sessions.rs`, `harness/turns.rs` | `an_edge_without_home_reads_as_this_hosts_ask`, `an_ask_ref_round_trips_through_needs_json` |
| Q2 room relay | `crates/room/src/{chat,server,wire,room}.rs`, new `crates/room/src/relay.rs`, `crates/room/tests/relay.rs` | `a_relay_seat_needs_the_operator_token`, `a_moving_ask_is_open_and_never_escalated`, `an_adopted_ask_keeps_its_home_and_seq`, `asks_reads_local_and_mirrored_asks_as_one_list`, `a_reply_from_a_host_the_directory_does_not_place_it_on_is_refused`, `protocol_11_is_refused` |
| Q3 hub | new `crates/harnessd/src/room/hub.rs`, `crates/harnessd/src/room/coordinator.rs` (start the hub sweep), `crates/tokencore/src/store/migrations.rs` (the journal, schema 42) | `hub::presence_from_two_hosts_is_merged_by_session`, `hub::an_unreachable_host_escalates_with_the_host_sentence`, `a_store_gains_the_migration_journal` |
| Q4 migrate | `crates/letibot/src/migrate.rs`, `crates/letibot/tests/migrate.rs` | `a_migration_hands_off_and_re_homes_open_asks`, `a_failed_migration_unhands_and_settles_nothing`, `a_resumed_migration_continues_from_the_journal_phase`, `the_destination_gets_a_coordinator_before_the_session_starts` |
| Q5 name resolution | `crates/harnessd/src/room/link.rs`, `crates/tools/src/builtins/room.rs` (refusal and schema text) | `a_bare_name_resolves_on_this_host_then_the_directory`, `the_same_name_on_two_hosts_is_refused_with_both_ids` |
| Q6 e2e | `crates/harnessd/tests/room_cross_host_e2e.rs` | `an_asker_migrates_and_its_answer_still_releases_its_row`, `a_target_migrates_and_answers_from_the_new_host`. Two coordinators in two scratch dirs stand in for two hosts, the relay over a unix socket instead of ssh. |
| Q7 docs | `docs/room.md`, `docs/remote-hosts.md`, `docs/answer-waits.md` (drop the same-host note) | none |

Q2 and Q3 touch `crates/harnessd/src/room/` only in Q3's files. Q5 is after Q2. Q4 is after Q2 and Q3.
Fleet tests (lab2x1 and lubuntu1 over real ssh) run after Q6 with `act`, not in CI.

## Open questions for the operator

1. Which box is the hub? Default: the box that runs `--migrate` and holds `hosts.toml`, the
   operator's local box, because it already is the index of record. Alternative: a `hub = "<host>"`
   key in `hosts.toml` for an always-on box, at the cost of that box holding ssh keys to the others.
2. A laptop hub sleeps. Do cross-host asks escalate while the hub is away? Default: no. Hosts keep
   their last mirror, same-host asks run, and the hub's first sweep after waking escalates what is
   past the grace.
3. Should a re-homed ask, both ends back on one host, return to that host's room or stay at the hub
   until it settles? Default: return. Same-host asks then never depend on the hub.
4. Is the repo id the first root commit, or the `origin` remote URL? Default: root commit. It is in
   every bundle, and a remote URL differs between a clone over ssh and one over https.
5. Do Claude Code seats (`cc@...`) take part across hosts? Default: no. A seat is a bridge on one
   box, it does not migrate, and its asks stay same-host as R136.
