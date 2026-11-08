#!/usr/bin/env bash
# shellcheck disable=SC2015 # every check here is `COND && note || fail`; note and fail both answer 0, so the arms are exclusive
# **What can be proved about the Claude Code plugin without Claude Code.** The plugin (TODO.md
# R117, `claude-plugin/`) is two JSON files, two skills and one line of `install.sh`, so the suite
# walks those and nothing here starts `claude`: the manifest names the server the install puts on
# PATH, the channel binds to a server that exists, every skill the plugin carries is one Claude
# Code can load, and the line the docs print is the line `install.sh --help` prints.
#
#   tests/claude-plugin.sh      # from the repo root; exits non-zero on a failure
set -u
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
PLUGIN="$ROOT/claude-plugin"
MANIFEST="$PLUGIN/.claude-plugin/plugin.json"
MARKET="$ROOT/.claude-plugin/marketplace.json"
REPO_SLUG="deadtrickster/letibot"

fails=0
note() { echo "ok   $1"; }
fail() {
  echo "FAIL $1"
  [ -n "${2:-}" ] && printf '%s\n' "$2" | sed 's/^/     /'
  fails=$((fails + 1))
}

# The document parses as JSON at all. A manifest that does not is a plugin that does not load, and
# a suite that only greps it would still pass.
json_ok() { python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$1" 2>/dev/null; }

# field FILE EXPR [WORDS] -- one value out of the document, as text. EXPR is a literal in this
# file with the document bound to `d` (and WORDS, split, as sys.argv[3]); it is never built from a
# variable, so nothing here evaluates input.
field() {
  python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
v = eval(sys.argv[2])
if v is None:
    print("")
elif isinstance(v, bool):
    print("true" if v else "false")
elif isinstance(v, list):
    print(",".join(str(x) for x in v))
else:
    print(v)
' "$1" "$2" "${3:-}" 2>/dev/null
}

# has TEXT PATTERN -- TEXT holds PATTERN, compared as a plain substring.
has() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac }

# ---------------------------------------------------------------------------------------
# The plugin manifest.
if json_ok "$MANIFEST"; then
  note "the plugin manifest parses"
else
  fail "the plugin manifest parses" "$(python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$MANIFEST" 2>&1)"
  echo "claude-plugin: the manifest does not parse, the rest cannot be judged"
  exit 1
fi
json_ok "$MARKET" && note "the marketplace file parses" ||
  fail "the marketplace file parses" "$(python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$MARKET" 2>&1)"

name="$(field "$MANIFEST" 'd.get("name")')"
[ "$name" = leticode ] && note "the plugin is named leticode" || fail "the plugin is named leticode" "name is '$name'"
case "$name" in
  *[[:space:]/:@]*) fail "the plugin name is kebab-case" "name is '$name'" ;;
  *) note "the plugin name is kebab-case" ;;
esac

# `claude plugin validate` warns without these three, and the listing then shows blanks.
for want in description version; do
  v="$(field "$MANIFEST" "d.get('$want')")"
  [ -n "$v" ] && note "the manifest states $want" || fail "the manifest states $want" "absent"
done
[ -n "$(field "$MANIFEST" 'd.get("author", {}).get("name")')" ] && note "the manifest names an author" ||
  fail "the manifest names an author" "absent"

# **The plugin's version is the workspace's.** A version here PINS the plugin until it changes, so
# a number left behind is every user frozen on the first one they installed — the drift
# `check-tag.sh` exists for, one file over.
ws="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' "$ROOT/Cargo.toml" | head -1)"
pv="$(field "$MANIFEST" 'd.get("version")')"
[ -n "$ws" ] && [ "$pv" = "$ws" ] && note "the plugin version is the workspace's ($ws)" ||
  fail "the plugin version is the workspace's" "plugin is '$pv', [workspace.package] is '$ws'"

# **The command is the bridge, by bare name.** `letibot-mcp` is what `install.sh` puts in
# `~/.local/bin`, so the manifest names it rather than a path only one box has.
servers="$(field "$MANIFEST" '",".join(d.get("mcpServers", {}).keys())')"
[ "$servers" = leticode ] && note "the plugin declares one MCP server, leticode" ||
  fail "the plugin declares one MCP server, leticode" "mcpServers holds '$servers'"
command="$(field "$MANIFEST" 'd["mcpServers"]["leticode"]["command"]')"
[ "$command" = letibot-mcp ] && note "the server runs letibot-mcp" ||
  fail "the server runs letibot-mcp" "command is '$command'"
has "$command" " " && fail "the command holds no space (it is resolved from PATH)" "$command" ||
  note "the command holds no space (it is resolved from PATH)"
args="$(field "$MANIFEST" 'd["mcpServers"]["leticode"].get("args", [])')"
[ -z "$args" ] && note "the server needs no arguments" || fail "the server needs no arguments" "args are '$args'"

# The channel binds to a server of THIS plugin. The entry is a strict object — Claude Code refuses
# the whole plugin for an unknown key inside one — so the two keys it may carry are named.
chan_server="$(field "$MANIFEST" 'd.get("channels", [{}])[0].get("server", "")')"
[ "$chan_server" = leticode ] && note "the channel binds to the server leticode" ||
  fail "the channel binds to the server leticode" "channels[0].server is '$chan_server'"
has ",$servers," ",$chan_server," && note "  ...which the plugin declares" ||
  fail "  ...which the plugin declares" "mcpServers are '$servers'"
chan_keys="$(field "$MANIFEST" '",".join(sorted(d.get("channels", [{}])[0].keys()))')"
[ "$chan_keys" = "displayName,server" ] && note "the channel entry carries only server and displayName" ||
  fail "the channel entry carries only server and displayName" "keys are '$chan_keys'"

# The room waiter: the plugin's half of what used to be a per-seat Monitor command.
monitor="$(field "$MANIFEST" 'd.get("experimental", {}).get("monitors", [{}])[0].get("command", "")')"
has "$monitor" "letibot-mcp room-wait" && note "the room waiter is a monitored room-wait" ||
  fail "the room waiter is a monitored room-wait" "'$monitor'"
has "$monitor" "--attention to_me" && note "  ...on messages for this seat" ||
  fail "  ...on messages for this seat" "'$monitor'"
has "$(field "$MANIFEST" 'd.get("experimental", {}).get("monitors", [{}])[0].get("name", "")')" "room-waiter" &&
  note "  ...named room-waiter" || fail "  ...named room-waiter" "unnamed or renamed"

# **A top-level key Claude Code does not read is stripped with a warning, not refused**, so a typo
# would load and do nothing. The list is the manifest reference's Fields table.
allowed="\$schema name displayName version description author homepage repository license keywords metadata icon documentationUrl supportUrl privacyPolicyUrl termsOfServiceUrl defaultEnabled dependencies settings userConfig types channels skills commands agents hooks mcpServers lspServers outputStyles workflows experimental"
unknown="$(field "$MANIFEST" '",".join(sorted(k for k in d if k not in sys.argv[3].split()))' "$allowed")"
if [ -z "$unknown" ]; then
  note "every top-level key is one Claude Code reads"
else
  fail "every top-level key is one Claude Code reads" "unknown: $unknown"
fi

# ---------------------------------------------------------------------------------------
# The marketplace: its one entry must resolve to this plugin, from a path inside this repository,
# because the plugin is installed from the repository and not from a copy of it.
market="$(field "$MARKET" 'd.get("name")')"
[ -n "$market" ] && note "the marketplace is named ($market)" || fail "the marketplace is named" "absent"
[ -n "$(field "$MARKET" 'd.get("owner", {}).get("name")')" ] && note "the marketplace names an owner" ||
  fail "the marketplace names an owner" "absent"
entries="$(field "$MARKET" '",".join(p.get("name", "") for p in d.get("plugins", []))')"
[ "$entries" = "$name" ] && note "the marketplace's one entry is the plugin" ||
  fail "the marketplace's one entry is the plugin" "entries are '$entries', plugin is '$name'"
source="$(field "$MARKET" 'd["plugins"][0].get("source", "")')"
case "$source" in
  ./*) note "the entry's source is a relative path ($source)" ;;
  *) fail "the entry's source is a relative path" "source is '$source'" ;;
esac
if has "$source" ".."; then
  fail "the source does not escape the marketplace" "$source"
else
  note "the source holds no '..'"
fi
[ -f "$ROOT/${source#./}/.claude-plugin/plugin.json" ] && note "  ...holding the plugin's manifest" ||
  fail "  ...holding the plugin's manifest" "no .claude-plugin/plugin.json under $ROOT/${source#./}"

# ---------------------------------------------------------------------------------------
# The skills the plugin carries: a directory of `<name>/SKILL.md`, with frontmatter Claude Code
# reads — the opening `---` has to be the first line, and the description is what it loads by.
skills=""
for dir in "$PLUGIN"/skills/*/; do
  [ -d "$dir" ] || continue
  skill="$(basename "$dir")"
  skills="$skills $skill"
  file="$dir/SKILL.md"
  if [ ! -f "$file" ]; then
    fail "the $skill skill has a SKILL.md" "nothing at $file"
    continue
  fi
  [ "$(head -1 "$file")" = "---" ] && note "the $skill skill opens with frontmatter" ||
    fail "the $skill skill opens with frontmatter" "its first line is '$(head -1 "$file")'"
  desc="$(sed -n 's/^description: *//p' "$file" | head -1)"
  [ -n "$desc" ] && note "  ...and a description Claude can load it by" ||
    fail "  ...and a description Claude can load it by" "no description: line in $file"
done
for want in leticode room-waiter; do
  has " $(printf '%s' "$skills") " " $want " && note "the plugin carries the $want skill" ||
    fail "the plugin carries the $want skill" "skills are:$skills"
done
# The plugin uses the default `skills/` scan, so there is no second path to drift. A `skills` key
# here would be a path Claude Code checks at load time (`Path not found`), and this suite would have
# to walk it too - so adding one is a deliberate act that has to reach this file.
declared="$(field "$MANIFEST" '",".join(d.get("skills", []))')"
[ -z "$declared" ] && note "the skills are the default skills/ scan, with no path to drift" ||
  fail "the skills are the default skills/ scan" "the manifest declares skills: $declared"

# ---------------------------------------------------------------------------------------
# The install. `letibot-mcp` on PATH is what makes the command above resolvable, and the one line a
# person types is read from `install.sh --help` rather than restated here.
bins="$(sed -n 's/^BINARIES="\(.*\)"$/\1/p' "$ROOT/install.sh" | head -1)"
has " $bins " " letibot-mcp " && note "install.sh puts letibot-mcp on PATH" ||
  fail "install.sh puts letibot-mcp on PATH" "BINARIES is '$bins'"
arch="$(sed -n 's/^ARCHIVE_BINARIES="\(.*\)"$/\1/p' "$ROOT/scripts/make-dist.sh" | head -1)"
has " $arch " " letibot-mcp " && note "the release archive carries it" ||
  fail "the release archive carries it" "ARCHIVE_BINARIES is '$arch'"

help="$(sh "$ROOT/install.sh" --help 2>&1)"
st=$?
[ "$st" -eq 0 ] && note "install.sh --help exits 0" || fail "install.sh --help exits 0" "exit $st"
line="/plugin install $name --marketplace $REPO_SLUG"
has "$help" "$line" && note "  ...and prints the one install line" ||
  fail "  ...and prints the one install line" "$help"
flag="claude --dangerously-load-development-channels plugin:$name@$market"
has "$help" "$flag" && note "  ...and the development flag the channel still needs" ||
  fail "  ...and the development flag the channel still needs" "$help"
has "$help" "server:$name" && note "  ...and the bare-registration spelling beside it" ||
  fail "  ...and the bare-registration spelling beside it" "$help"

for said in "$ROOT/README.md" "$PLUGIN/README.md"; do
  if grep -qF -- "$line" "$said"; then
    note "${said#"$ROOT"/} says the same line"
  else
    fail "${said#"$ROOT"/} says the same line" "no '$line' in it"
  fi
done

# ---------------------------------------------------------------------------------------
[ "$fails" -eq 0 ] || {
  echo "$fails check(s) failed"
  exit 1
}
echo "claude-plugin: the manifests, the skills and the install line all agree"
