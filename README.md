# IO Gateway

Local multi-provider AI gateway that rotates multiple accounts (Codex, Gemini, Qwen, DeepSeek, MiniMax, GLM, Grok, Copilot, Claude, and more) behind one shared API key.

## Who is this app for

People with multiple Codex accounts who want to share usage evenly from a single Codex CLI setup.

## What it does

- Accepts Codex CLI requests on a local port.
- Validates a single shared proxy key.
- Rotates upstream Codex tokens in round‑robin for even usage.
- Forwards requests to the Codex backend with required headers.

## Dashboard

Open `http://127.0.0.1:8319/` to see per‑account usage and errors.

If `admin_auth.enabled` is on, the dashboard requires:
- the configured admin API key
- a 6-digit TOTP code from Google Authenticator

## Codex Login (Web UI)

Open `http://127.0.0.1:8319/` and scroll to **Codex OAuth Login**.

Flow:
1. Click **Start Login** (it will open a new tab; if blocked, copy the URL shown).
2. Complete login in the new tab.
3. Copy the callback URL (it will fail to connect on the server).
4. Paste the callback URL into the form and click **Submit**.
5. The credential is saved into `auth_dir` and immediately loaded.

## Files

- `config.json` – runtime config (create from `config.example.json`).
- `auths/` – Codex JSON credentials (copied from your deployed AIProxyAPI auth dir).
- `src/main.rs` – Rust gateway implementation.

## Setup

1. Copy and edit config:

```bash
cp config.example.json config.json
```

2. Put Codex credential files in `auths/` (type `codex`, containing `access_token`).

3. Set your shared proxy key in your shell:

```bash
export CODEX_GATEWAY_KEY="your-random-key"
```

4. Configure admin login for dashboard/account management.

You can either put the values in `config.json`:

```json
"admin_auth": {
  "enabled": true,
  "api_key": "your-admin-api-key",
  "totp_secret": "BASE32_SECRET_FROM_GOOGLE_AUTHENTICATOR",
  "session_ttl_seconds": 43200
}
```

Or set them with environment variables:

```bash
export ADMIN_AUTH_ENABLED=true
export ADMIN_AUTH_API_KEY="your-admin-api-key"
export ADMIN_AUTH_TOTP_SECRET="BASE32_SECRET_FROM_GOOGLE_AUTHENTICATOR"
export ADMIN_AUTH_SESSION_TTL_SECONDS=43200
```

`totp_secret` must be a Base32 TOTP secret that Google Authenticator can import.

5. Run the gateway:

```bash
cargo run
```

## Config

`config.json`:

```json
{
  "listen": "0.0.0.0:8319",
  "upstream_base": "https://chatgpt.com/backend-api/codex",
  "proxy_api_key": "your-shared-proxy-key",
  "tokens": [],
  "auth_dir": "/root/dev/yow/gpt-gateway/auths",
  "admin_auth": {
    "enabled": true,
    "api_key": "your-admin-api-key",
    "totp_secret": "BASE32_SECRET_FROM_GOOGLE_AUTHENTICATOR",
    "session_ttl_seconds": 43200
  },
  "oauth": {
    "providers": {
      "qwen": {
        "client_id": "f0304373b74a44d2b584a3fb70ca9e56",
        "client_secret": "",
        "redirect_uri": "http://127.0.0.1:8319/login/qwen/callback",
        "scopes": ["openid", "profile", "email", "model.completion"],
        "authorize_url": "https://chat.qwen.ai/oauth/authorize",
        "token_url": "https://chat.qwen.ai/api/v1/oauth2/token",
        "validate_url": "https://chat.qwen.ai/api/v1/auths/",
        "refresh_url": "https://chat.qwen.ai/api/v1/auths/",
        "session_url": "https://chat.qwen.ai/api/v1/auths/",
        "base_url": "https://portal.qwen.ai/v1"
      },
      "claude": {
        "client_id": "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
        "redirect_uri": "https://platform.claude.com/oauth/code/callback",
        "scopes": ["org:create_api_key", "user:profile", "user:inference", "user:sessions:claude_code", "user:mcp_servers", "user:file_upload"],
        "authorize_url": "https://claude.com/cai/oauth/authorize",
        "token_url": "https://platform.claude.com/v1/oauth/token",
        "base_url": "https://api.anthropic.com"
      }
    }
  }
}
```

Notes:
- `tokens` is optional. If empty, tokens are loaded from `auth_dir`.
- Tokens are de‑duplicated and rotated in round‑robin order.
- `admin_auth.api_key` falls back to `proxy_api_key` if left empty, but using a separate key is safer.
- `admin_auth.totp_secret` can also be provided by `ADMIN_AUTH_TOTP_SECRET`.
- `admin_auth.session_ttl_seconds` defaults to 12 hours.
- `oauth.providers.qwen` and `oauth.providers.claude` are optional. Built-in defaults are used when omitted, and any field can also be overridden with `QWEN_OAUTH_*` or `CLAUDE_OAUTH_*` environment variables from `.env.example`.

## Qwen Browser Token Flow

This gateway now follows the same Qwen login model used by [`encryptarun/qwen-api`](https://github.com/encryptarun/qwen-api): it validates a browser token from `chat.qwen.ai` instead of sending the user into a normal OAuth authorize/callback flow.

Local setup:

1. Start the gateway with `cargo run`.
2. Open `http://127.0.0.1:8319/login/qwen/start`.
3. The helper page will tell you to open `https://chat.qwen.ai`, sign in, and run the browser token extractor against `localStorage.token`.
4. Paste that token back into the helper page, or paste it into the dashboard Qwen modal.
5. Confirm a `type: "qwen"` auth file appears under `auth_dir`, then verify the account with `curl http://127.0.0.1:8319/qwen/accounts.json`.

Relevant Qwen environment variables for this flow:

- `QWEN_OAUTH_VALIDATE_URL`
- `QWEN_OAUTH_REFRESH_URL`
- `QWEN_OAUTH_SESSION_URL`
- `QWEN_OAUTH_BASE_URL`

Operational notes:

- The local helper route is `GET /login/qwen/start`. It serves instructions and the extractor snippet; it does not redirect into `https://chat.qwen.ai/oauth/authorize`.
- The usable Qwen API base for responses/models is normalized to `https://portal.qwen.ai/v1`. Older saved credentials that still point at `https://chat.qwen.ai/api/v1` are remapped at runtime.
- Direct token submission still uses `POST /login/qwen/start` with `{"token":"..."}`.
- Browser-token-backed Qwen accounts refresh through the upstream `/auths/` session endpoint and keep the original browser token unless the upstream explicitly returns a replacement refresh token.
- The legacy device-code flow is no longer used.

## Codex CLI config

Example entry in `~/.codex/config.toml`:

```toml
[model_providers.codex_gateway]
name = "Local IO Gateway"
base_url = "http://127.0.0.1:8319"
env_key = "CODEX_GATEWAY_KEY"
wire_api = "responses"
requires_openai_auth = false

[profiles.codex_gateway]
model = "gpt-5.2-codex"
model_provider = "codex_gateway"
```

Then run Codex CLI with that profile.

## Quick API test with curl

List available models:

```bash
curl http://127.0.0.1:8319/v1/models \
  -H "Authorization: Bearer $CODEX_GATEWAY_KEY"
```

Send a basic text request:

```bash
curl http://127.0.0.1:8319/v1/responses \
  -H "Authorization: Bearer $CODEX_GATEWAY_KEY" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json" \
  --data '{
    "model": "gpt-5.2",
    "input": "Write a one-line hello from IO Gateway."
  }'
```

Generate an image and save the streamed PNG to `/tmp/codex-gateway.png`:

```bash
tmp=$(mktemp)
curl -sS -N http://127.0.0.1:8319/v1/responses \
  -H "Authorization: Bearer $CODEX_GATEWAY_KEY" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  --data '{
    "model": "gpt-5.2",
    "input": "Create a simple red square icon on a white background.",
    "tools": [{"type": "image_generation"}],
    "stream": true
  }' > "$tmp"

sed -n 's/^data: //p' "$tmp" \
  | jq -r 'select(.type=="response.image_generation_call.partial_image") | .partial_image_b64' \
  | tail -n 1 \
  | base64 -d > /tmp/codex-gateway.png
```

## Provider Routing

The public API surface is source-oriented:

- `GET /v1/models` returns the unified model catalog across enabled providers with provider-prefixed model ids.
- `POST /v1/responses` is the primary OpenAI-compatible execution endpoint.
- `POST /v1/chat/completions` is also available for provider-prefixed targets that expose native chat completions, including GLM.
- The gateway picks the target adapter from the `model` id instead of exposing provider-specific `/provider/v1/*` APIs.

Current routing rules:

- `gpt-*` and unmatched models go to the Codex target.
- `qwen*` goes to Qwen.
- `deepseek*` goes to DeepSeek.
- `grok*` goes to Grok.
- `claude*` goes to the native Claude OAuth target.
- `glm*` goes to GLM/Z.AI.
- Standard `gemini-*` models go to the native Gemini target.
- Antigravity-only Gemini variants such as `gemini-3-pro-image`, `gemini-3-pro-high`, `gemini-3-pro-low`, and `gemini-2.5-flash-thinking` go to Antigravity.

This means clients should stay on `/v1/*` and switch providers by changing only the `model` field. Use the prefixed `id` values returned by `/v1/models` when building model pickers or saved client config.

Provider prefixes can force a specific target while keeping the upstream model id unchanged. Prefixes are exactly three letters and must not include whitespace after the colon. The gateway strips the prefix before forwarding to the provider:

- `agw:gemini-2.5-pro` routes to Antigravity with upstream model `gemini-2.5-pro`.
- `gem:gemini-2.5-pro` routes to the native Gemini target with upstream model `gemini-2.5-pro`.
- Supported prefixes: `agw` Antigravity, `gem` Gemini, `qwn` Qwen, `dsk` DeepSeek, `grk` Grok, `min` MiniMax, `cop` GitHub Copilot, `cld` Claude, `glm` GLM/Z.AI, `cod` Codex/OpenAI.
- Old unprefixed model names still work exactly as before.

### Custom models

Custom models expose a stable alias such as `ctm:workhorse` and route it to one or more real provider models. They are configured from the admin dashboard Custom model section or the admin-only API below. The gateway stores them in `custom-models.json` under `auth_dir`; no database is required.

Custom model aliases are returned by `GET /v1/models`, `GET /models`, and `GET /codex/models`. They can be used from `POST /v1/responses`, `POST /codex/responses`, `POST /claude/messages`, and `POST /claude/responses`.

Save or replace an alias:

```bash
curl -sS http://127.0.0.1:8319/custom-models/save \
  -b "$ADMIN_COOKIE" \
  -H "Content-Type: application/json" \
  --data '{
    "alias": "workhorse",
    "display_name": "Workhorse",
    "enabled": true,
    "load_balance": true,
    "primary_models": [
      {"model": "agw:gemini-2.5-pro"},
      {"model": "gem:gemini-2.5-pro"}
    ],
    "fallback_models": [
      {"model": "min:MiniMax-M3"},
      {"model": "agw:gpt-oss"}
    ]
  }'
```

Call it like any other model:

```bash
curl -sS http://127.0.0.1:8319/v1/responses \
  -H "Authorization: Bearer $CODEX_GATEWAY_KEY" \
  -H "Content-Type: application/json" \
  --data '{"model":"ctm:workhorse","input":"Reply with OK only."}'
```

Delete an alias:

```bash
curl -sS http://127.0.0.1:8319/custom-models/delete \
  -b "$ADMIN_COOKIE" \
  -H "Content-Type: application/json" \
  --data '{"alias":"workhorse"}'
```

Custom model rules:

- `alias` is normalized by trimming whitespace and removing an optional `ctm:` prefix. `workhorse` and `ctm:workhorse` refer to the same alias.
- Aliases must not be empty and must not contain whitespace, `:`, `/`, or `\`.
- A custom model must have at least one enabled primary target.
- Targets may use these provider prefixes: `agw`, `gem`, `qwn`, `dsk`, `grk`, `min`, `cop`, `cld`, `glm`, and `cod`. Unprefixed targets still use the normal provider routing rules.
- Targets cannot point at another custom model, so recursive `ctm:` routes are rejected.
- Disabled custom models are hidden from model catalogs and return `503` if called directly.
- Disabled primary or fallback target entries are skipped.
- When `load_balance` is `true`, enabled primary targets are sorted by provider/account usage score and rotated on ties. The optional target `weight` lowers that target's usage score before comparison, so a higher weight makes it more likely to be selected when other signals are similar.
- When `load_balance` is `false`, enabled primary targets are tried in the configured order.
- Fallback targets are always appended after primary targets and are tried in configured order.
- The gateway falls back only when the selected target returns HTTP status `400` or higher. Successful responses, including short or model-truncated responses, are returned as-is.
- If every target fails, the response is `502` with the failed target list in the error message.
- Saving an existing alias replaces it. To rename, send `original_alias` or `previous_alias`; the old alias is removed after the new one is saved.

Rule coverage was manually verified on the live server with curl: admin auth, validation errors, load-balancing rotation, ordered routing, disabled target skipping, fallback chaining, all-target failure, disabled model behavior, alias normalization, rename, catalog exposure, file-backed persistence after service restart, and cleanup.

Generate an image through Antigravity using the unified `/v1/responses` endpoint:

```bash
tmp=$(mktemp)
curl -sS -N http://127.0.0.1:8319/v1/responses \
  -H "Authorization: Bearer $CODEX_GATEWAY_KEY" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  --data '{
    "model": "gemini-3-pro-image",
    "input": "Create a flat orange cat icon on a white background.",
    "tools": [{"type": "image_generation"}],
    "stream": true
  }' > "$tmp"

sed -n 's/^data: //p' "$tmp" \
  | jq -r 'select(.type=="response.image_generation_call.partial_image") | .partial_image_b64' \
  | tail -n 1 \
  | base64 -d > /tmp/antigravity-image.png
```

## Image Input Support

The gateway forwards image parts from Codex Responses API requests to every upstream target. The client can attach images using either of these content-part shapes:

```json
{
  "type": "input_image",
  "image_url": "data:image/png;base64,...."
}
```

```json
{
  "type": "image_url",
  "image_url": { "url": "https://example.com/x.png" }
}
```

Each target translates the input into its own native content shape:

- **Codex** (passthrough) — native Responses API, no translation.
- **Grok** (passthrough) — native Responses API, no translation.
- **MiniMax / Qwen** — OpenAI Chat-Completions content array `[{type: "text", text: ...}, {type: "image_url", image_url: {url: ...}}]`.
- **DeepSeek** — Anthropic Messages content array with `text` blocks and `image` blocks (base64 or url source).
- **Gemini** — Google Generative `parts` array with `text`, `inline_data` (data URL), or `file_data` (remote URL).
- **Antigravity** — same as Gemini, but routed through the Antigravity Cloud endpoint.

Text-only requests are still flattened to a string `content` for backward compatibility — only requests that contain image parts produce a content array.

### Model compatibility on this server

This list reflects the models exposed by the live `/v1/models` endpoint on the gateway host (`DEPLOY_HOST:DEPLOY_PORT` as of the latest deploy) and whether each one actually accepted an attached image in a smoke test against a real screenshot.

#### ✅ Receives image correctly

| Provider | Models |
|---|---|
| minimax | `MiniMax-M3` (note: `MiniMax-M2.7` and the older `MiniMax-M2.x` line are text-only) |
| openai (codex) | `codex-auto-review`, `gpt-5.4`, `gpt-5.4-mini`, `gpt-5.5` |
| gemini | `gemini-2.5-pro`, `gemini-2.5-flash`, `gemini-2.5-flash-lite`, `gemini-3-pro` |
| antigravity | `gemini-2.5-flash-thinking`, `gemini-3.1-flash-image`, `gemini-3.1-flash-lite`, `gemini-3.1-pro-low`, `gemini-3.3-flash-extra-low`, `gemini-3.3-flash-low`, `gemini-3-flash-agent`, `gemini-pro-agent`, `claude-sonnet-4-6`, `claude-opus-4-6-thinking` |
| qwen | `qwen3-vl-plus` (dedicated VL model), `qwen3-coder-plus`, `qwen3.5-plus`, `qwen3.5-flash`, `qwen3.5-max-2026-03-08`, `qwen3.5-27b`, `qwen3.5-omni-flash`, `qwen3.5-omni-plus`, `qwen3.5-397b-a17b`, `qwen3.6-plus`, `qwen3.6-plus-preview`, `qwen3.6-max-preview`, `qwen3.7-plus`, `qwen3.7-max`, `qwen-latest-series-invite-beta-v16`, `qwen-latest-series-invite-beta-v24`, `qwen-plus-2025-07-28`, `qwen3-omni-flash-2025-12-01` |
| deepseek | `deepseek-v4-pro`, `deepseek-v4-flash` |

#### ❌ Cannot receive image (model itself is text-only)

| Provider | Models | Why |
|---|---|---|
| minimax | `MiniMax-M2`, `MiniMax-M2.1`, `MiniMax-M2.1-highspeed`, `MiniMax-M2.5`, `MiniMax-M2.5-highspeed`, `MiniMax-M2.7`, `MiniMax-M2.7-highspeed` | Only `MiniMax-M3` supports vision. The M2.x family replies "Cannot see image. Please describe." |

#### ⚠️ Upstream model is not available in this account (gateway forwards correctly, upstream 404s/400s)

These are not gateway bugs — the request reaches the upstream, the upstream just doesn't have the model. Pick a different model in the same family.

| Model | Upstream error |
|---|---|
| `gemini-2.5-flash-image-preview` | Gemini 404 — not in the configured project |
| `gemini-3-flash` | Gemini 404 — not in the configured project |
| `gemini-3.1-pro-high` | Antigravity 400 — not in the antigravity catalog |
| `gemini-3-pro-image` | Antigravity 404 — not in the antigravity catalog |

#### ⚠️ Upstream explicitly rejects image (model doesn't support vision)

| Model | Why |
|---|---|
| `gpt-5.3-codex-spark` | OpenAI returns 400 "Model 'gpt-5.3-codex-spark' does not support image inputs". Use `gpt-5.4`, `gpt-5.5`, etc. for vision. |

#### 🔑 Auth / permission issue (image support not tested)

| Model | Why |
|---|---|
| `gpt-oss-120b-medium` | The current Codex account isn't entitled to this model ("not supported when using Codex with a ChatGPT account"). |
| All `grok-*` models | The Grok OAuth token expired on `2026-06-19T14:16:17+00:00`. Re-auth the Grok account on the dashboard (`/admin` → Grok → re-auth) to restore. Once refreshed, the standard grok chat models accept image input. |

#### Best models for image input on this server

Ranked by quality of the answer in the smoke test against the same screenshot:

1. `gemini-2.5-pro`
2. `gpt-5.5`
3. `claude-sonnet-4-6`
4. `claude-opus-4-6-thinking`
5. `qwen3-vl-plus` (Qwen's dedicated vision-language model)
6. `qwen3.7-plus`
7. `MiniMax-M3`

#### Agentic loop status per provider (does Codex CLI get to drive it to completion?)

The "agentic loop" check is the same one `codex` does on the desktop: send a
multi-step task ("create file, cat it, delete it"), and let the model keep
calling the `shell` tool turn after turn until it claims the work is done.
A provider passes when the model actually invokes the tool (and re-invokes it
after a synthetic tool result) instead of hallucinating completion.

| Provider | Agentic loop | Notes |
|---|---|---|
| `codex` (gpt-5.5, gpt-5.4, gpt-5.4-mini, codex-auto-review) | ✅ | Native Codex backend, the reference behaviour. |
| `MiniMax-M3` | ✅ | Routed through MiniMax's native Responses endpoint for Codex and the Anthropic endpoint for Claude Code. |
| `MiniMax-M2.7` and the other M2.x line | ⚠️ | Text-only — returns "Cannot see image" but does iterate tool calls. |
| `deepseek-v4-pro` / `deepseek-v4-flash` | ✅ | Goes through Anthropic messages; tool calls and reasoning are translated. |
| `gemini-2.5-pro`, `gemini-2.5-flash`, `gemini-2.5-flash-lite`, `gemini-3-pro` | ✅ | After the gateway forwards `functionDeclarations` and translates `functionCall` parts. |
| `antigravity` (`claude-sonnet-4-6`, `claude-opus-4-6-thinking`, `gemini-*-thinking`, `gemini-*-image`, …) | ✅ | Same fix as Gemini — tools forwarded, function_call items emitted. |
| `qwen3-coder-plus`, `qwen3.5-plus`, `qwen3.6-plus`, `qwen3.7-plus`, `qwen3.7-max` | ✅ | Previously the gateway was dropping the `tools` array so qwen hallucinated completion; now it forwards them to `qwen.aikit.club/v1/chat/completions` and renders the `tool_calls` back into Responses-API function_call items. |
| `qwen3-vl-plus` and other dedicated VL qwen models | ✅ | Image input + tool use both work. |
| `grok-4.3`, `grok-4.20-...` | 🔑 | Gateway forwards tools correctly but the OAuth token expired on `2026-06-19T14:16:17+00:00`; re-auth on the dashboard to restore. |

## DeepSeek

DeepSeek still uses the same source-facing `/v1/*` API after you add credentials.

- Add a DeepSeek account from the homepage at `http://127.0.0.1:8319/`
- Or submit a key directly to `POST /login/deepseek/start` as JSON:

```json
{
  "api_key": "sk-...",
  "label": "optional",
  "base_url": "https://api.deepseek.com"
}
```

List DeepSeek models:

```bash
curl http://127.0.0.1:8319/v1/models \
  -H "Authorization: Bearer $CODEX_GATEWAY_KEY"
```

Send a basic DeepSeek request through the unified Responses bridge:

```bash
curl http://127.0.0.1:8319/v1/responses \
  -H "Authorization: Bearer $CODEX_GATEWAY_KEY" \
  -H "Content-Type: application/json" \
  --data '{
    "model": "deepseek-v4-pro",
    "input": "Reply with one short line."
  }'
```

Notes:
- The gateway validates the key against DeepSeek `GET /models` before saving it.
- Tool-call turns and reasoning summaries are translated into DeepSeek’s chat-completions format so Codex CLI can keep using the Responses API against the gateway instead of talking to DeepSeek directly.

### DeepSeek balance on the dashboard

Each DeepSeek account card on the dashboard shows the current balance pulled from `GET /user/balance` (cached for 60 seconds per account). The bar shows `Balance USD` with the `total_balance` (and the `topped_up_balance` / `granted_balance` breakdown if one of them is non-zero). If the upstream is unreachable the card shows the upstream error text instead.

## Troubleshooting

- `403 cloudflare`: usually missing headers or wrong upstream. Use the provided gateway build.
- `Instructions are required`: your payload is too minimal (Codex CLI sends proper instructions).
- `502 Bad Gateway`: port collision or proxy isn’t running.

## MiniMax provider

MiniMax is exposed as an OpenAI-compatible target. Add a MiniMax API key through the dashboard, or `POST /login/minimax/start` with JSON `{"api_key":"...","label":"optional","base_url":"optional"}`.

Public routing (no `/minimax/*` routes are exposed to clients):

- `POST /v1/responses` and `POST /codex/responses` route MiniMax models to MiniMax's native `/v1/responses` endpoint and forward the Codex Responses body without translating it to chat completions.
- `POST /claude/v1/messages` and `POST /claude/messages` route `MiniMax-*` models to MiniMax's Anthropic-compatible `/anthropic/v1/messages` endpoint and forward the Claude Messages body as-is.
- If an account's `base_url` is explicitly configured to end with `/v1/chat/completions` or `/chat/completions`, the gateway keeps the old chat-completions fallback for OpenAI/Codex Responses requests.
- `GET /v1/models` and `GET /codex/models` include live MiniMax models such as `MiniMax-M3`, `MiniMax-M2.7`, and `MiniMax-M2.7-highspeed` whenever a MiniMax account is enabled.
- Auth: `Authorization: Bearer <proxy_api_key>` or `x-api-key: <proxy_api_key>`.

### MiniMax usage and quota on the dashboard

The dashboard pulls the same numbers shown on <https://platform.minimax.io/console/usage> for each MiniMax account:

* `GET /v1/api/openplatform/coding_plan/remains` is called on every account refresh; the response is cached for 60 seconds.
* The two large progress bars on the MiniMax card are the **5-hour rolling window** and the **weekly window** that the platform console highlights. The bar fill is `used_percent = 100 − remaining_percent`; the label is `used_percent% used · resets in <reset_label>` (e.g. `12.3% used · resets in 4h 38m`).
* Below the headline bars, a per-model breakdown is shown for the same windows so you can see which model in the M-series is driving the usage.
* If the upstream returns `base_resp.status_msg` (e.g. `account is not subscribed`) the message is shown beneath the bars.
* If the account key is invalid the card shows the upstream error text instead of a bar.

Examples:

```bash
curl http://127.0.0.1:8319/v1/responses \
  -H "Authorization: Bearer $CODEX_GATEWAY_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"MiniMax-M3","input":"say hi in one word"}'
```

Streaming:

```bash
curl http://127.0.0.1:8319/v1/responses \
  -H "Authorization: Bearer $CODEX_GATEWAY_KEY" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{"model":"MiniMax-M3","input":"say hi","stream":true}'
```

### Using MiniMax with the Codex CLI

The gateway is a drop-in replacement for the `model_provider` in `~/.codex/config.toml` that the official MiniMax docs describe (https://platform.minimax.io/docs/token-plan/codex). Add the following to `config.toml`:

```toml
[model_providers.minimax_via_gateway]
name = "MiniMax via IO Gateway"
base_url = "http://DEPLOY_HOST:DEPLOY_PORT/v1"
experimental_bearer_token = "<CODEX_GATEWAY_KEY>"
wire_api = "responses"

[profiles.minimax]
model = "MiniMax-M3"
model_provider = "minimax_via_gateway"
model_context_window = 1000000
model_reasoning_effort = "high"
```

The model catalog fields documented at the link above (`default_reasoning_level`, `input_modalities: ["text", "image"]`, `supports_parallel_tool_calls: true`, etc.) all work end-to-end through the gateway.

### Using MiniMax with Claude Code

The gateway can also stand in for MiniMax's Anthropic endpoint from the official Claude Code guide (https://platform.minimax.io/docs/token-plan/claude-code). Configure Claude Code with the gateway as the Anthropic base URL:

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://DEPLOY_HOST:DEPLOY_PORT/claude",
    "ANTHROPIC_AUTH_TOKEN": "<CODEX_GATEWAY_KEY>",
    "CLAUDE_CODE_AUTO_COMPACT_WINDOW": "1000000",
    "ANTHROPIC_MODEL": "MiniMax-M3[1m]",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "MiniMax-M3[1m]",
    "ANTHROPIC_DEFAULT_OPUS_MODEL": "MiniMax-M3[1m]",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "MiniMax-M3[1m]"
  }
}
```

## GLM/Z.AI provider

GLM is exposed as a Z.AI API-key provider with two account types:

- `api_usage`: normal Z.AI API usage keys. This is the default. OpenAI/Codex requests use `https://api.z.ai/api/paas/v4`; Claude Messages requests are translated through Chat Completions.
- `subscription`: GLM Coding Plan subscription keys. OpenAI/Codex requests use `https://api.z.ai/api/coding/paas/v4`; Claude Messages requests pass through to `https://api.z.ai/api/anthropic`.

Add a GLM account from the dashboard, or submit a key directly:

```bash
curl -sS http://127.0.0.1:8319/login/glm/start \
  -b "$ADMIN_COOKIE" \
  -H "Content-Type: application/json" \
  -d '{
    "api_key": "ZAI_API_KEY",
    "label": "personal",
    "account_type": "api_usage"
  }'
```

Routing:

- Use the `glm:` prefix to force GLM, for example `glm:glm-5.2`.
- Unprefixed `glm*` model ids also route to GLM.
- `POST /v1/responses` and `POST /codex/responses` translate OpenAI/Codex Responses input, tools, tool output, and streaming events to GLM's OpenAI-compatible `/chat/completions` route.
- `POST /v1/chat/completions` passes OpenAI Chat Completions requests through to GLM's OpenAI-compatible route.
- `POST /claude/v1/messages` and `POST /claude/messages` use the account type: API-usage accounts translate Anthropic Messages to Chat Completions and synthesize Anthropic responses; subscription accounts pass Anthropic Messages through to GLM's Anthropic-compatible route.
- `GET /v1/models` and `GET /codex/models` include `glm:` model ids whenever a GLM account is enabled.
- The dashboard shows the live model catalog from `/models`. Z.AI does not currently expose a stable GLM quota endpoint through this route, so account load balancing uses gateway-recorded usage first.

For subscription keys, keep the Coding Plan base URLs separate in saved credentials: Anthropic Messages uses `https://api.z.ai/api/anthropic`, while OpenAI Chat Completions uses `https://api.z.ai/api/coding/paas/v4`.

Example:

```bash
curl http://127.0.0.1:8319/v1/responses \
  -H "Authorization: Bearer $CODEX_GATEWAY_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"glm:glm-5.2","input":"say hi in one word"}'
```

Native chat completions:

```bash
curl http://127.0.0.1:8319/v1/chat/completions \
  -H "Authorization: Bearer $CODEX_GATEWAY_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"glm:glm-5.2","messages":[{"role":"user","content":"say hi"}]}'
```

### Using GLM with the Codex CLI

```toml
[model_providers.glm_via_gateway]
name = "GLM via IO Gateway"
base_url = "http://127.0.0.1:8319/v1"
experimental_bearer_token = "<CODEX_GATEWAY_KEY>"
wire_api = "responses"

[profiles.glm]
model = "glm:glm-5.2"
model_provider = "glm_via_gateway"
model_reasoning_effort = "high"
```

### Using GLM with Claude Code

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:8319/claude",
    "ANTHROPIC_AUTH_TOKEN": "<CODEX_GATEWAY_KEY>",
    "ANTHROPIC_MODEL": "glm:glm-5.2",
    "ANTHROPIC_SMALL_FAST_MODEL": "glm:glm-5.2"
  }
}
```

## GitHub Copilot provider

GitHub Copilot is exposed as an OpenAI Responses target and as an Anthropic Messages bridge for Claude Code. Add a Copilot account from the dashboard, or start the device-code flow with:

```bash
curl -X POST http://127.0.0.1:8319/login/copilot/start \
  -H "Authorization: Bearer $CODEX_GATEWAY_KEY" \
  -H "Content-Type: application/json" \
  -d '{"account_type":"individual"}'
```

Open the returned GitHub verification URL, enter the user code, then submit the returned `device_code`:

```bash
curl -X POST http://127.0.0.1:8319/login/copilot/submit \
  -H "Authorization: Bearer $CODEX_GATEWAY_KEY" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  -d 'device_code=DEVICE_CODE'
```

Public routing:

- Use the `cop:` prefix so Copilot GPT models do not collide with the native Codex target. The gateway strips the prefix before calling Copilot.
- `POST /v1/responses` and `POST /codex/responses` forward the Responses body to Copilot `/responses`.
- `POST /claude/v1/messages` and `POST /claude/messages` translate Claude Messages to Copilot Responses and return Anthropic-compatible output.
- `GET /v1/models` and `GET /codex/models` include Copilot models when a Copilot account is enabled.
- The dashboard reads Copilot quota snapshots from GitHub's Copilot user metadata and live model availability from the Copilot `/models` endpoint.
- Copilot model metadata includes `model_picker_category`, `policy_state`, `billing_tier`, `premium`, `utility_model`, and `app_accessible`. The gateway only exposes Copilot models that were manually verified through this app's `/v1/responses` path; inaccessible Copilot `/models` entries are filtered out. The verified Copilot set is currently GPT-3.5 Turbo, GPT-4.1, GPT-4o, and GPT-4o mini variants, all observed as non-premium because premium-interaction quota did not decrease during curl tests.

Example:

```bash
curl http://127.0.0.1:8319/v1/responses \
  -H "Authorization: Bearer $CODEX_GATEWAY_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"cop:gpt-5.1","input":"say hi in one word"}'
```

### Using GitHub Copilot with the Codex CLI

```toml
[model_providers.copilot_via_gateway]
name = "GitHub Copilot via IO Gateway"
base_url = "http://127.0.0.1:8319/v1"
experimental_bearer_token = "<CODEX_GATEWAY_KEY>"
wire_api = "responses"

[profiles.copilot]
model = "cop:gpt-5.1"
model_provider = "copilot_via_gateway"
model_reasoning_effort = "high"
```

### Using GitHub Copilot with Claude Code

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:8319/claude",
    "ANTHROPIC_AUTH_TOKEN": "<CODEX_GATEWAY_KEY>",
    "ANTHROPIC_MODEL": "cop:claude-sonnet-4",
    "ANTHROPIC_SMALL_FAST_MODEL": "cop:claude-sonnet-4"
  }
}
```

## Claude OAuth provider

Claude is exposed as a native Anthropic OAuth target. Add a Claude account from the dashboard with the same pattern as Codex login: click **Start Login**, complete Claude login in the browser, copy the displayed `CODE#STATE` value or callback URL, and paste it back into the dashboard. The gateway stores the PKCE verifier server-side and saves only the OAuth token file under `auth_dir`.

```bash
curl -sS 'http://127.0.0.1:8319/login/claude/start?label=personal' \
  -b "$ADMIN_COOKIE"
```

Open the returned `url`, finish login, then submit the displayed code or callback URL:

```bash
curl -sS http://127.0.0.1:8319/login/claude/submit \
  -b "$ADMIN_COOKIE" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  --data-urlencode 'redirect_url=CODE#STATE'
```

Cookie fallback is still supported when browser OAuth is unavailable:

```bash
curl -sS http://127.0.0.1:8319/login/claude/start \
  -b "$ADMIN_COOKIE" \
  -H "Content-Type: application/json" \
  -d '{"cookie":"CLAUDE_AI_COOKIE","label":"personal","organization_uuid":"required-for-cookie-fallback"}'
```

Direct token fallback is also supported when you already have trusted Anthropic OAuth tokens:

```bash
curl -sS http://127.0.0.1:8319/login/claude/start \
  -b "$ADMIN_COOKIE" \
  -H "Content-Type: application/json" \
  -d '{"access_token":"OAUTH_ACCESS_TOKEN","refresh_token":"OAUTH_REFRESH_TOKEN","label":"personal"}'
```

Public routing:

- Use the `cld:` prefix when you want to force Claude, for example `cld:claude-sonnet-4-20250514`.
- Unprefixed `claude*` model ids also route to the native Claude target.
- `POST /v1/responses` and `POST /codex/responses` translate OpenAI Responses input/tools to Anthropic Messages and map the reply back to Responses format.
- `POST /claude/v1/messages` and `POST /claude/messages` pass Anthropic Messages through to the Anthropic API using the saved OAuth token.
- `GET /v1/models` and `GET /codex/models` include `cld:` model ids when a Claude account is enabled.
- Access tokens are refreshed from the saved refresh token when they expire.

Example:

```bash
curl http://127.0.0.1:8319/v1/responses \
  -H "Authorization: Bearer $CODEX_GATEWAY_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"cld:claude-sonnet-4-20250514","input":"say hi in one word"}'
```

### Using Claude with the Codex CLI

```toml
[model_providers.claude_via_gateway]
name = "Claude via IO Gateway"
base_url = "http://127.0.0.1:8319/v1"
experimental_bearer_token = "<CODEX_GATEWAY_KEY>"
wire_api = "responses"

[profiles.claude]
model = "cld:claude-sonnet-4-20250514"
model_provider = "claude_via_gateway"
model_reasoning_effort = "high"
```

### Using Claude with Claude Code

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:8319/claude",
    "ANTHROPIC_AUTH_TOKEN": "<CODEX_GATEWAY_KEY>",
    "ANTHROPIC_MODEL": "cld:claude-sonnet-4-20250514",
    "ANTHROPIC_SMALL_FAST_MODEL": "cld:claude-3-5-haiku-20241022"
  }
}
```
