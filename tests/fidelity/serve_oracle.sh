#!/usr/bin/env bash
# Bring up the /apply-template oracle the W3 gate diffs against.
#
# TWO THINGS THAT ARE NOT OPTIONAL
#
# 1. It must be a DIRECTLY LAUNCHED single-model server. `/apply-template` is
#    proxied in router mode (server.cpp:239 maps post_apply_template to
#    models_routes->proxy_post), so pointing the gate at the router tests the
#    router. That is UNVERIFIED-6 and this is the day-one workaround for it.
#
# 2. It costs no inference. `/apply-template` runs the same
#    oaicompat_chat_params_parse as /v1/chat/completions and touches no slot, no
#    sequence and no GPU (server-context.cpp:7745). That is why this can run in CI
#    on every commit.
#
# MODE: substitute (default) — a small model carrying GLM's template
# ------------------------------------------------------------------
# GLM-5.3-Flash is 199.7 GB and this box does not have room for it beside the Qwen
# that is serving on 8080 (13 GB free on GPU0, 108 GB of host RAM). So the oracle is
# a ~0.8 GB model started with --chat-template-file pointed at GLM's own jinja, on
# CPU, on a port of its own.
#
# What that changes, exhaustively:
#
#   * `bos_token` / `eos_token` jinja globals come from the substitute's vocab.
#     GLM's template references NEITHER (grep it), so nothing reads them.
#   * common_chat_template_direct_apply strips a leading bos_token when the vocab
#     says add_bos. GLM's GGUF has no tokenizer.ggml.add_bos_token key and a BPE
#     vocab defaults to add_bos=false (llama-vocab.cpp:1815, :2600), so no strip
#     happens for GLM — and the substitute's bos is not "[gMASK]" either, so no
#     strip happens here.
#   * Everything else the render depends on — the template source, the caps derived
#     from it, the message normalisation, the tool-schema reconstruction, the jinja
#     engine — is model-independent code in the same binary.
#
# So the substitute is faithful for template fidelity. It is NOT faithful for token
# ids, and this rig deliberately does not test those: RenderSpan is text and control
# tokens precisely so that this check needs no vocab.
#
# MODE: real — `MODE=real serve_oracle.sh start`
# ----------------------------------------------
# Uses ~/bin/glm-flash-server on the same free port. Correct when the box has room.
# Everything above stops mattering.

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
PORT="${PORT:-8137}"
MODE="${MODE:-substitute}"
TEMPLATE="${TEMPLATE:-$HERE/../../crates/dialect-glm/template/glm-5.3-flash.jinja}"
LLAMA="${LLAMA_BIN:-$HOME/Projects/llama.cpp/build-glm/bin/llama-server}"
SUB_MODEL="${SUB_MODEL:-$HOME/models/vl/minicpm46-Q8_0.gguf}"
SUB_MMPROJ="${SUB_MMPROJ:-$HOME/models/vl/mmproj-minicpm46.gguf}"
RUN="${RUN_DIR:-${TMPDIR:-/tmp}/letibot-fidelity}"
LOG="$RUN/oracle-$PORT.log"
PIDFILE="$RUN/oracle-$PORT.pid"

die() { echo "serve_oracle: $*" >&2; exit 1; }

start() {
  mkdir -p "$RUN"
  if curl -sf -m 2 "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
    echo "already up on $PORT"; return 0
  fi
  [ -x "$LLAMA" ] || die "no llama-server at $LLAMA (set LLAMA_BIN)"

  if [ "$MODE" = real ]; then
    [ -x "$HOME/bin/glm-flash-server" ] || die "no ~/bin/glm-flash-server"
    # Never on 8080: that port is serving, and evicting it costs somebody a turn.
    [ "$PORT" = 8080 ] && die "refusing to start on 8080"
    PORT="$PORT" nohup "$HOME/bin/glm-flash-server" >"$LOG" 2>&1 &
  else
    [ -f "$TEMPLATE" ] || die "no template at $TEMPLATE"
    [ -f "$SUB_MODEL" ] || die "no substitute model at $SUB_MODEL (set SUB_MODEL)"
    local args=(
      -m "$SUB_MODEL"
      --alias glm-template-oracle --host 127.0.0.1 --port "$PORT"
      -ngl 0 -c 4096 --threads 4
      --jinja --chat-template-file "$TEMPLATE"
      # A prefix truncated mid-assistant-turn must render as history, not as a
      # continuation: with prefill on, the server rewrites the last assistant
      # message into a continuation and drops the closing </think>.
      --no-prefill-assistant
    )
    [ -f "$SUB_MMPROJ" ] && args+=( --mmproj "$SUB_MMPROJ" --no-mmproj-offload )
    LD_LIBRARY_PATH="${CUDA_LIB:-/usr/local/cuda-13.3/lib64}:${LD_LIBRARY_PATH:-}" \
      CUDA_VISIBLE_DEVICES="" nohup "$LLAMA" "${args[@]}" >"$LOG" 2>&1 &
  fi
  echo $! >"$PIDFILE"

  for _ in $(seq 1 90); do
    if curl -sf -m 2 "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
      echo "oracle up on $PORT (mode=$MODE, log $LOG)"; return 0
    fi
    sleep 2
  done
  tail -20 "$LOG" >&2
  die "oracle did not come up on $PORT; see $LOG"
}

stop() {
  [ -f "$PIDFILE" ] || { echo "no pidfile $PIDFILE"; return 0; }
  # Kill by recorded pid, never by pattern: a pgrep -f on the port matches the shell
  # that is doing the matching.
  kill "$(cat "$PIDFILE")" 2>/dev/null || true
  rm -f "$PIDFILE"
  echo "stopped"
}

case "${1:-start}" in
  start) start ;;
  stop) stop ;;
  status) curl -sf -m 2 "http://127.0.0.1:$PORT/health" && echo || echo "down" ;;
  *) die "usage: $0 {start|stop|status}" ;;
esac
