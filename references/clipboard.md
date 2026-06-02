# clipboard — read/write the target-side text buffer

A two-way text channel between the agent and the target, stored on the Pi
(persists across reboots, 64 KB cap). USB HID can't touch the target's *OS*
clipboard, so this is how you hand long credentials, tokens, log snippets, or
context to the target: stage the text here, then **type it onto the target via
HID**. Load this when you need that. All calls assume `https://${AEON_HOST}/`
with HTTP Basic `$AEON_USER:$AEON_PASSWD`.

## read the buffer

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" "https://$AEON_HOST/api/clipboard"
# → {"ok":true,"text":"…","size_bytes":N,"max_bytes":65536}
```

## write the buffer

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X PUT \
    -H "Content-Type: application/json" \
    -d '{"text": "secret-token-xyz"}' \
    "https://$AEON_HOST/api/clipboard"
# → {"ok":true,"size_bytes":N,"trimmed":false}
```

Replaces existing content; anything over 64 KB is truncated (`trimmed:true`).

## type the buffer onto the target (HID)

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    "https://$AEON_HOST/api/clipboard/type-on-target"
# → {"ok":true,"typed":24,"skipped":2,"input_chars":26}
```

Types the stored buffer through the HID keyboard, character by character.
Skips unmappable characters (emoji, smart quotes) and reports `typed` /
`skipped` / `input_chars` counts so you can detect data loss. **Focus the
target's input field first** (move + click — see `references/input.md`), then
fire this.

`what` → stage text on-device, then type it onto the target. `why` over
typing the credential directly: it persists if you need to retry, and (under a
restrictive token scope) the value need not stay in your prompt history after
staging.

## MCP

Over MCP the tools are `get_clipboard`, `set_clipboard`, and `type_clipboard`
(the last maps to `/api/clipboard/type-on-target`). See `references/mcp.md`.

> Route note: the REST verbs are `GET` and **`PUT`** on `/api/clipboard`, and
> `POST /api/clipboard/type-on-target`. The MCP tool names (`set_clipboard`,
> `type_clipboard`) abstract those.
