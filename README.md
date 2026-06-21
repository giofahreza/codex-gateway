# codex-gateway

Simple local proxy for Codex CLI that rotates multiple Codex accounts behind one shared API key.

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
- `oauth.providers.qwen` is optional. Built-in defaults are used when omitted, and any field can also be overridden with `QWEN_OAUTH_*` environment variables from `.env.example`.

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
name = "Local Codex Gateway"
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
    "input": "Write a one-line hello from Codex Gateway."
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

- `GET /v1/models` returns the unified model catalog across enabled providers.
- `POST /v1/responses` is the single OpenAI-compatible execution endpoint.
- The gateway picks the target adapter from the `model` id instead of exposing provider-specific `/provider/v1/*` APIs.

Current routing rules:

- `gpt-*` and unmatched models go to the Codex target.
- `qwen*` goes to Qwen.
- `deepseek*` goes to DeepSeek.
- `grok*` goes to Grok.
- `claude*` goes to Antigravity.
- Standard `gemini-*` models go to the native Gemini target.
- Antigravity-only Gemini variants such as `gemini-3-pro-image`, `gemini-3-pro-high`, `gemini-3-pro-low`, and `gemini-2.5-flash-thinking` go to Antigravity.

This means clients should stay on `/v1/*` and switch providers by changing only the `model` field.

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

```

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

## Troubleshooting

- `403 cloudflare`: usually missing headers or wrong upstream. Use the provided gateway build.
- `Instructions are required`: your payload is too minimal (Codex CLI sends proper instructions).
- `502 Bad Gateway`: port collision or proxy isn’t running.

## MiniMax provider

MiniMax is exposed as an OpenAI-compatible target. Add a MiniMax API key through the dashboard, or `POST /login/minimax/start` with JSON `{"api_key":"...","label":"optional","base_url":"optional"}`.

Public routing (the gateway translates to MiniMax internally; no `/minimax/*` routes are exposed to clients):

- `POST /v1/responses` and `POST /codex/responses` translate the request to MiniMax's `/v1/chat/completions` by default and add `thinking: {type: "adaptive"}` so `MiniMax-M3` uses Adaptive Thinking on agentic tasks. The Codex SDK's `reasoning.effort` is mapped to MiniMax's `thinking.type` (`high` → `adaptive`, `none` → `disabled`). The `apply_patch` tool is filtered, and Codex-only fields (`store`, `include`, `parallel_tool_calls`, …) are dropped.
- If the account's `base_url` is configured to end with `/v1/responses` the gateway uses MiniMax's native `/v1/responses` endpoint instead. The native path is a closer passthrough, but `MiniMax-M3` tends to stop after a single tool call on it, so the chat-completions path is the default.
- `GET /v1/models` and `GET /codex/models` include live MiniMax models such as `MiniMax-M3`, `MiniMax-M2.7`, and `MiniMax-M2.7-highspeed` whenever a MiniMax account is enabled.
- Auth: `Authorization: Bearer <proxy_api_key>`.

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
name = "MiniMax via codex-gateway"
base_url = "http://DEPLOY_HOST:DEPLOY_PORT/v1"
experimental_bearer_token = "<CODEX_GATEWAY_KEY>"
wire_api = "responses"

[profiles.minimax]
model = "MiniMax-M3"
model_provider = "minimax_via_gateway"
model_context_window = 512_000
model_reasoning_effort = "high"
```

The model catalog fields documented at the link above (`default_reasoning_level`, `input_modalities: ["text", "image"]`, `supports_parallel_tool_calls: true`, etc.) all work end-to-end through the gateway.

### Why the gateway uses chat-completions by default for MiniMax

`MiniMax-M3` is significantly more thorough on multi-step agentic tasks when invoked through the `/v1/chat/completions` endpoint with `thinking: {type: "adaptive"}` than through the native `/v1/responses` endpoint. On the native Responses API path, the model often stops after a single tool call and returns `response.completed`, which the Codex agent loop reads as "task done" even when the work is not finished. The chat-completions path instead produces a longer, more thorough response (multiple tool calls in a single turn, or one comprehensive script that covers the whole task) and the Codex agent loop is able to drive the work to completion.

The gateway does the protocol translation:

* `reasoning: {effort: "..."}` from the Codex SDK is mapped to `thinking: {type: "..."}`. `effort: "none"` becomes `thinking: {type: "disabled"}`; any other value (or absence) becomes `thinking: {type: "adaptive"}` for M3.
* The Codex Responses-API input format is translated to MiniMax's `messages` shape. Function calls and function results are mapped to OpenAI's `tool_calls` and `tool` role.
* The `apply_patch` tool is filtered (MiniMax does not implement it).
* Codex-only fields (`store`, `include`, `parallel_tool_calls`, `truncation`, `user`, `safety_identifier`) are dropped.
* The response stream is translated back to the OpenAI Responses-API SSE event sequence (`response.created`, `response.output_item.added`, `response.output_text.delta`, `response.function_call_arguments.delta`, `response.completed`, …) so the Codex SDK sees the same shape it would see from OpenAI's own Responses API.

MiniMax also exposes a native `/v1/responses` endpoint (https://platform.minimax.io/docs/api-reference/responses-create). The gateway uses that path instead when the account's `base_url` is configured to end with `/v1/responses`. That path is a closer passthrough, but in practice the chat-completions path gives much better multi-step agentic behavior.
