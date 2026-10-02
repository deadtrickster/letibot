#!/bin/sh
# Does each preset's `fallback_model` name a model its provider actually OFFERS?
#
# # Why this exists
#
# `Preset::fallback_model` is a frozen string used when the models.dev catalogue is
# not readable. Its doc comment has said, since the first time it went stale:
#
#   "Kept so a box without opencode still has a name to try rather than refusing,
#    and updated to a model that exists today — but it will go stale again"
#
# *Updated to a model that exists today* is a claim with a lifetime, and nothing
# measured it. MEASURED 2026-10-02, it had gone stale in a way that could not fail:
#
#   GET  /models                -> deepseek-flash, deepseek-v4-pro        (offered)
#   POST /chat/completions      -> also accepts deepseek-v4-flash and
#                                  deepseek-chat, answering `model: deepseek-flash`
#                                  for both                              (tolerated)
#
# so the frozen value had drifted to `deepseek-v4-flash` — a name the account does not
# offer — and NOTHING failed, because the API silently aliases it onto the model that
# does. An alias is a deprecation path: a fallback that works only because of one has
# a deadline nobody wrote down, on the path taken when something has already gone
# wrong. That is the failure this checks for.
#
# # What it checks, and what it deliberately does not
#
# It asks each provider's own `/models` and requires every preset's `fallback_model`
# to appear in the answer. It does NOT POST a completion: **accepted is not offered**,
# and it is the difference between the two that this exists to see.
#
# # A provider with no key is SKIPPED, loudly
#
# A key is a fact about the box, not a defect in the tree, so a missing one is not a
# failure — but it is printed, because a check that quietly passes when it did nothing
# is the thing this whole tree refuses. `LETIBOT_REQUIRE_PROVIDERS=1` turns those skips
# into failures, for a caller that knows keys are present (the same shape as
# `LETIBOT_REQUIRE_APPARATUS`).
#
# Usage:  sh scripts/check-provider-models.sh
set -eu

REQUIRE="${LETIBOT_REQUIRE_PROVIDERS:-}"

# The script's own directory, computed HERE rather than inside the heredoc: under `sh`
# the Python reads its program from stdin, so `__file__` is `<stdin>` and any path
# derived from it lands on the CALLER's directory — which made this look for
# `presets.rs` beside the cwd instead of beside the script.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

python3 - "$REQUIRE" "$here" <<'PY'
import json, os, re, sys, urllib.error, urllib.request

require = sys.argv[1] not in ("", "0")
here = sys.argv[2]

# One entry per preset, mirroring crates/provider/src/presets.rs. The `models`
# URL is the preset's own chat URL with the last path segment swapped: both
# DeepSeek and the OpenAI-shaped vendors put `/models` beside `/chat/completions`.
PRESETS = [
    # name      env names                                      file     models url
    ("deepseek", ["DEEPSEEK_API_KEY"],                          "deepseek",
     "https://api.deepseek.com/models"),
    ("glm",      ["ZHIPUAI_API_KEY", "ZHIPU_API_KEY", "ZAI_API_KEY", "GLM_API_KEY"], "glm",
     "https://open.bigmodel.cn/api/paas/v4/models"),
    ("grok",     ["XAI_API_KEY", "GROK_API_KEY"],               "grok",
     "https://api.x.ai/v1/models"),
]
# The frozen values, read out of the source rather than repeated here, so this
# cannot drift from the thing it is checking.
SRC = os.path.normpath(os.path.join(here, "..", "crates/provider/src/presets.rs"))

def fallbacks():
    text = open(SRC, encoding="utf-8").read()
    out = {}
    for const in ("DEEPSEEK", "GLM", "GROK"):
        m = re.search(r"pub const %s: Preset = Preset \{(.*?)\n\};" % const, text, re.S)
        if not m:
            continue
        name = re.search(r'name:\s*"([^"]+)"', m.group(1))
        fb = re.search(r'fallback_model:\s*"([^"]+)"', m.group(1))
        if name and fb:
            out[name.group(1)] = fb.group(1)
    return out

def key_for(envs, section):
    # The same order `crates/provider/src/keys.rs` documents: env, then the
    # operator's file, then opencode's store.
    for e in envs:
        if os.environ.get(e):
            return os.environ[e], "env $%s" % e
    cfg = os.path.expanduser("~/.config/letibot/providers.toml")
    if os.path.exists(cfg):
        text = open(cfg, encoding="utf-8").read()
        m = re.search(r"\[%s\](.*?)(?=\n\[|\Z)" % re.escape(section), text, re.S)
        if m:
            k = re.search(r'key\s*=\s*"([^"]+)"', m.group(1))
            if k:
                return k.group(1), "providers.toml [%s]" % section
    store = os.path.expanduser("~/.local/share/opencode/auth.json")
    if os.path.exists(store):
        e = json.load(open(store, encoding="utf-8")).get(section) or {}
        if e.get("type") == "api" and e.get("key"):
            return e["key"], "opencode auth.json"
    return None, None

def operator_price_models():
    """The model names the operator's own `[prices.*]` table prices.

    A dead key here is invisible: the lookup is `creds.prices.get(<model actually
    billed>)`, so an entry for a model nobody runs simply never matches and the cost
    quietly comes from the catalogue instead. That is *not* a wrong number — but the
    file then reads as though it is the price in force, which is how the operator's
    `[prices."deepseek-chat"]` came to price a model DeepSeek retired.
    """
    cfg = os.path.expanduser("~/.config/letibot/providers.toml")
    if not os.path.exists(cfg):
        return None
    text = open(cfg, encoding="utf-8").read()
    return re.findall(r'\[prices\.["\']?([^"\'\]]+)["\']?\]', text)


def main():
    fb = fallbacks()
    if not fb:
        print("check-provider-models: could not read any fallback_model from %s" % SRC)
        return 2

    bad = 0
    skipped = 0
    offered_by = {}
    for name, envs, section, url in PRESETS:
        want = fb.get(name)
        if want is None:
            print("  %-9s no fallback_model parsed — the source moved?" % name)
            bad += 1
            continue
        key, where = key_for(envs, section)
        if not key:
            print("  %-9s SKIPPED — no key in %s, providers.toml or opencode's store"
                  % (name, " / ".join(envs)))
            skipped += 1
            continue
        req = urllib.request.Request(url, headers={"Authorization": "Bearer %s" % key})
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                body = json.loads(r.read())
        except urllib.error.HTTPError as e:
            print("  %-9s HTTP %s from %s (%s) — a key that cannot list models is its own"
                  " problem, reported rather than skipped" % (name, e.code, url, where))
            bad += 1
            continue
        except Exception as e:
            print("  %-9s could not reach %s: %s" % (name, url, e))
            bad += 1
            continue
        offered = [m.get("id") for m in (body.get("data") or []) if m.get("id")]
        if not offered:
            print("  %-9s answered with no models, which is not a pass" % name)
            bad += 1
            continue
        offered_by[name] = set(offered)
        if want in offered:
            print("  %-9s fallback_model `%s` is offered  (%d total, via %s)"
                  % (name, want, len(offered), where))
        else:
            print("  %-9s fallback_model `%s` is NOT offered by %s" % (name, want, url))
            print("              offered: %s" % ", ".join(sorted(offered)))
            print("              a name the API merely ACCEPTS is an alias, which is a"
                  " deprecation path — see the field's doc comment")
            bad += 1

    # **The operator's own price table, for the same reason.** Every entry's model is
    # checked against whichever provider lists it; an entry nothing offers is dead
    # weight that reads as the price in force.
    prices = operator_price_models()
    if prices is not None:
        every = set().union(*offered_by.values()) if offered_by else set()
        dead = [p for p in prices if p not in every] if every else []
        if dead:
            print("  prices    the operator's table prices %d model(s) no consulted provider"
                  " offers: %s" % (len(dead), ", ".join(sorted(dead))))
            print("              harmless where it never matches (cost falls through to the"
                  " catalogue) but misleading: the entry reads as the price in force")
            # Reported, and deliberately NOT a failure: it is the operator's file, a
            # price may be a contract rate for a model this check cannot see, and the
            # cost is already correct by fall-through. The point is that it is SAID.
        elif every:
            print("  prices    every model the operator's table prices is offered")
        else:
            print("  prices    not checked — no provider answered")

    print()
    if bad:
        print("check-provider-models: %d provider(s) fail" % bad)
        return 1
    if skipped and require:
        print("check-provider-models: %d provider(s) skipped and LETIBOT_REQUIRE_PROVIDERS"
              " is set, so this is a failure" % skipped)
        return 1
    print("check-provider-models: ok%s"
          % (" (%d skipped, no key on this box)" % skipped if skipped else ""))
    return 0


sys.exit(main())
PY
