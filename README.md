# gpt-gateway

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

Open `http://127.0.0.1:8319/login`.

Flow:
1. Click “Start Login”.
2. Open the auth URL in your browser and complete login.
3. Copy the callback URL from the browser (it will fail to connect on the server).
4. Paste the callback URL into the form to save credentials into `auth_dir`.

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

## Troubleshooting

- `403 cloudflare`: usually missing headers or wrong upstream. Use the provided gateway build.
- `Instructions are required`: your payload is too minimal (Codex CLI sends proper instructions).
- `502 Bad Gateway`: port collision or proxy isn’t running.
