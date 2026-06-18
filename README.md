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

4. Run the gateway:

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
