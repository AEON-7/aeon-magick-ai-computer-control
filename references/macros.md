# macros & prompts — stored sequences and playbooks

Load this when you want to run a **deterministic keyboard-driven sequence**
(macros) or fetch a stored **agent playbook** (prompts). Macros are for the
moments that don't need vision; your reasoning loop drives the moments that do.
All calls assume `https://${AEON_HOST}/` with a Bearer token
`$AEON_TOKEN`.

## Macros — run a stored action sequence

```bash
# List
curl -sk -H "Authorization: Bearer $AEON_TOKEN" "https://$AEON_HOST/api/macros"

# Fetch one macro's TOML
curl -sk -H "Authorization: Bearer $AEON_TOKEN" "https://$AEON_HOST/api/macros/open-spotlight"

# Run with params
curl -sk -H "Authorization: Bearer $AEON_TOKEN" -X POST \
    -H "Content-Type: application/json" \
    -d '{"params": {"query": "Slack"}}' \
    "https://$AEON_HOST/api/macros/open-spotlight/run"
```

Shipped on the device: `open-spotlight`, `cmd-tab`, `screenshot-region`,
`see-then-click`.

Author your own by PUTing a TOML body to `/api/macros/<name>` (and
`DELETE /api/macros/<name>` to remove). User-authored macros at
`/etc/aeon/macros/` shadow shipped ones on name collision.

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" -X PUT --data-binary @my-macro.toml \
    "https://$AEON_HOST/api/macros/my-macro"
```

### Macro TOML schema

```toml
name = "open-spotlight"
description = "Open Spotlight, type a query, hit Enter."

[[params]]
name = "query"
required = true

[[steps]]
type = "key"
keys = ["GUI", "SPACE"]
hold_ms = 30

[[steps]]
type = "wait"
ms = 350

[[steps]]
type = "type"
text = "{{query}}"

[[steps]]
type = "key"
keys = ["ENTER"]
```

Step types: `type`, `key`, `click`, `move`, `scroll`, `wait`, `persona`,
`snapshot`, `release_all`. `{{var}}` placeholders in any string field are
replaced from the `params` object passed at run time. `snapshot` steps write
JPEGs to `/run/aeon/snapshots/` and the paths come back in the run response
under `snapshots`.

`what` → a named, parameterized sequence run in one call. `why` → cheaper and
more reliable than driving a fixed keyboard dance step-by-step; reserve your
vision loop for the parts that actually need eyes.

## Prompts — stored agent playbooks

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" "https://$AEON_HOST/api/prompts"
curl -sk -H "Authorization: Bearer $AEON_TOKEN" \
    "https://$AEON_HOST/api/prompts/agent-quickstart"
```

Shipped: `agent-quickstart`, `macos-shortcuts`. Useful to fetch at session
start to remind yourself of device idioms. Plain `.md`/`.txt`; author your own
with `PUT /api/prompts/<name>` (and `DELETE /api/prompts/<name>`).

Over MCP, macros and prompts are also surfaced as **resources**
(`aeon://macros/<name>`, `aeon://prompts/<name>`) and prompts answer
`prompts/list` + `prompts/get` — see `references/mcp.md`.
