# Evidence: what opencode sends for reasoning on the wire

Date: 2026-09-11. Question (docs/chat-templates.md §3, mechanism 2): does opencode
replay reasoning as its own message, fused onto the assistant message, or not at all?

**Answer: fused onto the assistant message as `reasoning_content`. It is never a
separate message.**

## 1. Provider config (opencode.jsonc)

The glm provider points DIRECTLY at the model server (the qwen-proxy on 8090 is
not in this provider's path):

    "glm": {
      "npm": "@ai-sdk/openai-compatible",
      "options": { "baseURL": "http://127.0.0.1:8080/v1", ... }
    }

No `reasoning` capability flag is declared for the provider; that flag gates
effort/UI presentation, not the replay shape below (the stream parser and the
message converter are unconditional).

## 2. Client-side storage (opencode.db)

opencode persists an assistant message's reasoning as SEPARATE parts, one per
reasoning block, alongside text parts. From this project's own session,
message msg_08d882d9b001pZBcXFxDR4Twh6 (model glm-5.3-flash):

    message data: {"parentID": "msg_08d67422b001mqMeTAC1G5Ws0F", "role": "assistant", "mode": "build", "agent": "build", "path": {"cwd": "/home/dead/Projects/ ...TRUNCATED
    parts: [{"snapshot": "d1a9fdb034d00ba539250a657dc55ee5988727fc", "type": "step-start"}, {"type": "reasoning", "text": "Only @ai-sdk/provider in node_modules \u2014 the openai-compatible adapter is embedded in the binary then (or under @opencode-ai). Check @opencode-ai contents. Also dump a reasoned assistant message: query messages joined with parts, find sessions in this project (project_directory table), get recent assistant messages whose parts include reasoning.", "time": {"start": 1789080908185, "end": 1789080912527}}, {"type": "tool", "tool": "bash", "callID": "4o4hTi9n1DWG0Kd8WKvfLATfHT5PXoq5" ...TRUNCATED

Queries to repeat:

    sqlite3 ~/.local/share/opencode/opencode.db   # or python3 sqlite3
    -- tables: message(id, session_id, data), part(message_id, data)
    -- part.data JSON has {"type":"reasoning","text":...} for reasoning spans.

## 3. The wire conversion (opencode binary, Bun-compiled, minified)

Inbound stream parsing — reasoning_content deltas become reasoning parts
(`OpenAIChat` step handler, binary strings):

    if(Y?.reasoning_content) N=w.reasoningDelta(N,J,"reasoning-0",Y.reasoning_content)

Outbound replay — `W.fn("OpenAIChat.lowerAssistantMessage")`, verbatim:

    return{role:"assistant",
           content:$.length===0?null:B.joinText($),
           tool_calls:Z.length===0?void 0:Z,
           reasoning_content:J.length>0?J.map((X)=>X.text).join("")
                            :a4(x.native?.openaiCompatible)}

All reasoning parts of one assistant turn are joined into ONE
`reasoning_content` field on the SAME assistant message object. There is no
code path in `OpenAIChat.lowerMessages` that emits reasoning as its own message;
the Responses-API converter (`OpenAIResponses.lowerMessages`, a different
protocol, not used by this provider) is the only one with per-item reasoning
messages. Fallback `a4(...)` reuses the model's native reasoning text from
provider metadata when no parts exist; same field, same message.

## 4. Consequence for §3

Mechanism 2 as filed (separate reasoning message -> extra turn marker the model
never produced) cannot fire with opencode: the message structure is exactly the
fused shape the OpenAI-compatible endpoint expects. The residual prompt-cache
divergence after the `interleaved` fix is therefore mechanism 1 (the template
leak) or a third, unidentified cause — not message structure.
