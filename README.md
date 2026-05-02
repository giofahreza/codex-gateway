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
  "auth_dir": "/root/dev/yow/gpt-gateway/auths"
}
```

Notes:
- `tokens` is optional. If empty, tokens are loaded from `auth_dir`.
- Tokens are de‑duplicated and rotated in round‑robin order.

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

## Troubleshooting

- `403 cloudflare`: usually missing headers or wrong upstream. Use the provided gateway build.
- `Instructions are required`: your payload is too minimal (Codex CLI sends proper instructions).
- `502 Bad Gateway`: port collision or proxy isn’t running.
