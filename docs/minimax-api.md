# MiniMax API Endpoints (`api.minimax.io`)

Reference of every endpoint reachable on `https://api.minimax.io` with a valid
`MINIMAX_API_KEY`, as probed on 2026-08-04. Each section includes a complete
`curl` example that was verified to return a 2xx.

## Auth

Two namespaces exist on the same host, each with a different auth header:

| Namespace | Auth header |
|---|---|
| `/anthropic/*` | `X-Api-Key: $MINIMAX_API_KEY` (the Anthropic SDK style; `Authorization: Bearer` is **rejected** here) |
| `/v1/*` | `Authorization: Bearer $MINIMAX_API_KEY` (OpenAI-style) |

The Anthropic-compat surface additionally requires `anthropic-version: 2023-06-01`.

## Models available

Same set returned by both `/anthropic/v1/models` and `/v1/models`:

- `MiniMax-M3`
- `MiniMax-M2.7` / `MiniMax-M2.7-highspeed`
- `MiniMax-M2.5` / `MiniMax-M2.5-highspeed`
- `MiniMax-M2.1` / `MiniMax-M2.1-highspeed`
- `MiniMax-M2`

---

## Anthropic-compatible namespace (`/anthropic/v1`)

### `GET /anthropic/v1/models`

```bash
curl -s "https://api.minimax.io/anthropic/v1/models" \
  -H "X-Api-Key: $MINIMAX_API_KEY" \
  -H "anthropic-version: 2023-06-01"
```

### `POST /anthropic/v1/messages`

```bash
curl -s "https://api.minimax.io/anthropic/v1/messages" \
  -X POST \
  -H "X-Api-Key: $MINIMAX_API_KEY" \
  -H "anthropic-version: 2023-06-01" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "MiniMax-M3",
    "max_tokens": 10,
    "messages": [
      { "role": "user", "content": "hi" }
    ]
  }'
```

### `POST /anthropic/v1/messages/count_tokens`

```bash
curl -s "https://api.minimax.io/anthropic/v1/messages/count_tokens" \
  -X POST \
  -H "X-Api-Key: $MINIMAX_API_KEY" \
  -H "anthropic-version: 2023-06-01" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "MiniMax-M3",
    "messages": [
      { "role": "user", "content": "hi" }
    ]
  }'
```

### Anthropic endpoints NOT implemented (404)

- `/anthropic/v1/messages/batches`
- `/anthropic/v1/files`
- `/anthropic/v1/organizations/me`
- `/anthropic/v1/skills`
- `/anthropic/v1/complete`

---

## Native + OpenAI-compat namespace (`/v1`)

### `GET /v1/models`

```bash
curl -s "https://api.minimax.io/v1/models" \
  -H "Authorization: Bearer $MINIMAX_API_KEY"
```

### `POST /v1/text/chatcompletion_v2` (native, recommended)

Native MiniMax chat endpoint. Response includes `reasoning_content`,
`audio_content`, and `reasoning_details`.

```bash
curl -s "https://api.minimax.io/v1/text/chatcompletion_v2" \
  -X POST \
  -H "Authorization: Bearer $MINIMAX_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "MiniMax-M3",
    "messages": [
      { "role": "user", "content": "hi" }
    ]
  }'
```

### `POST /v1/text/chatcompletion` (native v1)

```bash
curl -s "https://api.minimax.io/v1/text/chatcompletion" \
  -X POST \
  -H "Authorization: Bearer $MINIMAX_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "MiniMax-M3",
    "messages": [
      { "role": "user", "content": "hi" }
    ]
  }'
```

### `POST /v1/text/completion` (legacy completion)

```bash
curl -s "https://api.minimax.io/v1/text/completion" \
  -X POST \
  -H "Authorization: Bearer $MINIMAX_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "MiniMax-M3",
    "messages": [
      { "role": "user", "content": "hi" }
    ]
  }'
```

### `POST /v1/chat/completions` (OpenAI-compatible)

```bash
curl -s "https://api.minimax.io/v1/chat/completions" \
  -X POST \
  -H "Authorization: Bearer $MINIMAX_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "MiniMax-M3",
    "messages": [
      { "role": "user", "content": "hi" }
    ]
  }'
```

### `POST /v1/embeddings`

Returns a `vectors` field (different shape from OpenAI's `data[]`).

```bash
curl -s "https://api.minimax.io/v1/embeddings" \
  -X POST \
  -H "Authorization: Bearer $MINIMAX_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "embo-01",
    "texts": ["hi"]
  }'
```

### `GET /v1/files/list`

```bash
curl -s "https://api.minimax.io/v1/files/list" \
  -H "Authorization: Bearer $MINIMAX_API_KEY"
```

### `POST /v1/files/upload`

Multipart upload. Requires a real file payload.

```bash
curl -s "https://api.minimax.io/v1/files/upload" \
  -X POST \
  -H "Authorization: Bearer $MINIMAX_API_KEY" \
  -F "file=@/path/to/file"
```

### `POST /v1/files/delete`

```bash
curl -s "https://api.minimax.io/v1/files/delete" \
  -X POST \
  -H "Authorization: Bearer $MINIMAX_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{ "file_id": "<FILE_ID>" }'
```

### `GET /v1/batches`

```bash
curl -s "https://api.minimax.io/v1/batches" \
  -H "Authorization: Bearer $MINIMAX_API_KEY"
```

### `POST /v1/batches`

```bash
curl -s "https://api.minimax.io/v1/batches" \
  -X POST \
  -H "Authorization: Bearer $MINIMAX_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{}'
```

---

## Endpoints NOT available (404)

Probed but unreachable on `https://api.minimax.io`:

- Chat: `/v1/text/streamchat`, `/v1/vision`, `/v1/messages`, `/v1/text/anthropic`, `/v1/text/generation`
- Embeddings: `/v1/text/embedding`, `/v1/text/embedding_list`, `/v1/vector/create`
- Files: `/v1/files/retrieve`, `/v1/files/upload` (POST without file body)
- Audio: `/v1/audio/transcriptions`, `/v1/audio/translations`, `/v1/audio/speech`, `/v1/tts`
- Image: `/v1/image/generation`, `/v1/image/create`, `/v1/image/understand`
- Fine-tuning: `/v1/fine_tuning`, `/v1/fine_tuning/jobs`
- Assistants / threads
- Account: `/v1/user_info`, `/v1/account`, `/v1/api_key`, `/v1/usage`, `/v1/balance`, `/v1/user`
