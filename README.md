# IO Gateway

One local gateway for Codex CLI, Claude Code, and OpenAI-compatible clients. IO Gateway lets you connect multiple AI accounts, expose them through one API, load balance traffic across them, and manage access from a single dashboard.

Use one base URL. Switch provider by changing the model name.

```text
Codex CLI / Claude Code / OpenAI clients
              |
              v
        IO Gateway
              |
   +----------+----------+----------+
   v          v          v          v
 Codex     Claude     Gemini     Qwen ...
```

## What It Does

IO Gateway turns a pile of separate AI accounts into one operational gateway:

- One API surface for OpenAI Responses, OpenAI Chat Completions, and Anthropic Messages.
- Account-aware routing across Codex, Claude, Gemini, Antigravity, Qwen, DeepSeek, MiniMax, Grok, GitHub Copilot, and GLM.
- Load balancing and failover across multiple accounts.
- Custom model aliases that can route to one or many provider targets.
- Dashboard-based account, quota, usage, notification, and API-key management.
- Provider and account scoped API keys for controlled client access.

## Feature Highlights

### Unified Client Endpoint

Point clients at IO Gateway and keep your client setup stable:

- `POST /v1/responses`
- `POST /v1/chat/completions`
- `POST /claude/v1/messages`
- `GET /v1/models`

Models can be selected naturally by name, or forced to a provider with a prefix:

| Prefix | Provider |
|---|---|
| `cod:` | Codex / OpenAI via ChatGPT |
| `cld:` | Claude |
| `gem:` | Gemini |
| `agw:` | Antigravity |
| `qwn:` | Qwen |
| `dsk:` | DeepSeek |
| `min:` | MiniMax |
| `grk:` | Grok |
| `cop:` | GitHub Copilot |
| `glm:` | GLM / Z.AI |

Examples: `gpt-5.2`, `cld:claude-sonnet-4`, `gem:gemini-3-pro`, `qwn:qwen3.7-max`, `ctm:workhorse`.

### Custom Models

Create stable `ctm:` model aliases for your own routing strategy.

Custom models can:

- Route one alias to multiple providers.
- Pin a target to a specific account.
- Use every account except a selected account.
- Weight targets for load balancing.
- Fall back to another provider when a target fails.
- Appear in `/v1/models` like a normal model.

Example use cases:

- `ctm:workhorse` for day-to-day coding across several cheap or high-quota accounts.
- `ctm:fast` for low-latency models.
- `ctm:review` for a strict fallback chain from premium to backup providers.

### Load Balancing And Account Routing

Each provider can hold multiple accounts. IO Gateway tracks request load, quota state, recent failures, cooldowns, and active in-flight requests so traffic is distributed across eligible accounts.

The router is built for agent traffic:

- Avoid disabled and cooling-down accounts.
- Respect upstream quota and retry hints.
- Retry another account before meaningful output reaches the client.
- Track active streams so concurrent prompts spread across accounts.
- Refresh quota snapshots in the background.

### API Key Management

Create dashboard-managed API keys without giving dashboard access to every API user.

Keys can be:

- Unrestricted.
- Limited to one provider.
- Limited to multiple providers.
- Limited to selected accounts inside a provider.
- Updated or revoked from the dashboard.

Access rules are enforced before routing and load balancing, including custom-model requests.

### Dashboard

The dashboard is the control plane for the gateway:

- Add accounts through OAuth, device-code login, browser token extraction, or API key entry.
- See account cards with connection state, quota, package details, latest error details, and usage.
- Enable, disable, delete, refresh, and re-auth accounts.
- Switch account layout modes for dense provider views.
- Manage custom models.
- Manage API keys and scoped access.
- Configure notifications.
- Inspect usage charts and context history.

### Notifications

IO Gateway can alert you when accounts or model quotas need attention.

Supported channels:

- Telegram
- Google Chat webhook

Typical alerts:

- Account authentication or upstream errors.
- Model quota becomes fully used.
- Model quota becomes available again.

### Usage And Quota Visibility

The gateway records per-request usage and keeps operational snapshots for the dashboard.

Tracked data includes:

- Requests and errors.
- Input, output, cache, reasoning, and total tokens.
- Last seen, last success, and latest error details.
- Provider-specific quota windows and package data.
- Time-bucketed usage history for charts.

### Codex And Claude Friendly

IO Gateway is designed around real coding-agent workflows:

- Codex CLI can use the OpenAI Responses wire API through the gateway.
- Claude Code can use the Anthropic Messages endpoint through the gateway.
- Tool calls, function responses, image inputs, streaming, and agent loops are translated where providers need adapter logic.

## Supported Providers

| Provider | Prefix | Auth method |
|---|---|---|
| Codex / OpenAI via ChatGPT | `cod:` | OAuth browser redirect |
| Claude | `cld:` | PKCE OAuth |
| Gemini | `gem:` | Google OAuth |
| Antigravity | `agw:` | Google OAuth |
| Qwen | `qwn:` | Browser token extraction |
| DeepSeek | `dsk:` | API key |
| MiniMax | `min:` | API key |
| Grok | `grk:` | OAuth device code |
| GitHub Copilot | `cop:` | GitHub device code |
| GLM / Z.AI | `glm:` | API key |

## Quick Start

```bash
cargo build --release
cp config.example.json config.json
export CODEX_GATEWAY_KEY="your-shared-proxy-key"
./target/release/codex-gateway
```

Dashboard:

```text
http://127.0.0.1:8319/
```

Swagger API docs:

```text
http://127.0.0.1:8319/docs/
```

## Example Client Config

Codex CLI:

```toml
[model_providers.io_gateway]
name = "IO Gateway"
base_url = "http://127.0.0.1:8319"
env_key = "CODEX_GATEWAY_KEY"
wire_api = "responses"
requires_openai_auth = false

[profiles.gateway]
model = "ctm:workhorse"
model_provider = "io_gateway"
```

Claude Code:

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:8319/claude",
    "ANTHROPIC_AUTH_TOKEN": "<CODEX_GATEWAY_KEY>",
    "ANTHROPIC_MODEL": "cld:claude-sonnet-4"
  }
}
```

## Documentation

- [Docs Index](docs/README.md) - detailed documentation entry point.
- [Operator Guide](docs/operator-guide.md) - full setup, configuration, provider login, routing, API, deployment, testing, and troubleshooting docs.
- Runtime Swagger UI - available from a running gateway at `/docs/`.
- OpenAPI JSON - available from a running gateway at `/api-docs/openapi.json`.

## Build And Test

```bash
cargo fmt --check
cargo test --locked
cargo build --release --locked
```

Dashboard JavaScript syntax check:

```bash
awk '/<script>/{flag=1;next} /<\/script>/{flag=0} flag' src/main.rs > /tmp/io-gateway-dashboard.js
node --check /tmp/io-gateway-dashboard.js
```
