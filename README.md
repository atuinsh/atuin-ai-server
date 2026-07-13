# Atuin AI Server

A minimal self-hosted server for the Atuin AI protocol, backed by any
OpenAI-compatible chat-completions endpoint — Ollama, vLLM, LM Studio,
llama.cpp, LiteLLM, OpenRouter, or a compatible cloud API. It runs the
same engine the hosted Atuin AI service serves, composed with stateless
defaults: no accounts, no database, no usage limits, no recording.

The Atuin CLI talks to it unchanged: point `ai.endpoint` at this server
and chat runs against your own models.

## Quick start (Docker)

```sh
cp config.example.toml config.toml   # then edit
docker run \
  -v ./config.toml:/etc/atuin-ai/config.toml \
  -p 8080:8080 \
  atuin-ai-server
```

Building the image:

```sh
docker build -t atuin-ai-server .
```

To reach an engine running on the Docker host (e.g. local Ollama), use
`host.docker.internal` in the endpoint:

```toml
endpoint = "http://host.docker.internal:11434/v1"
```

## Quick start (from source)

Requires Erlang/OTP, Elixir, and Gleam (versions in the repository's
`.tool-versions`).

```sh
cp config.example.toml config.toml   # then edit
mix deps.get
mix run --no-halt
```

## Configuration

One TOML file, path given by `CHAT_CONFIG` (default `./config.toml`; the
Docker image defaults to `/etc/atuin-ai/config.toml`).

```toml
port = 8080

# The OpenAI-compatible chat-completions endpoint. Accepted with or
# without the trailing /chat/completions; a query string is passed
# through to every request.
endpoint = "http://localhost:11434/v1"

default_model = "llama"   # optional; defaults to the first model

[[models]]
alias = "llama"                       # what CLI users select
name = "Llama 3.2"                    # display name in the model list
description = "Local llama3.2 via Ollama"
model = "llama3.2:latest"             # sent as the request's `model`
```

The model (and serving engine) must support **tool calling** — the chat
protocol drives tools on every turn.

### API keys

Local engines usually need none — with no `api_key`, no Authorization
header is sent. Otherwise, three forms:

```toml
api_key = "sk-..."                    # inline

[api_key]
env = "CHAT_API_KEY"                  # from the environment

[api_key]
cmd = { run = "op", args = ["read", "op://vault/item/key"] }
                                      # from a command's stdout
```

By default the key is sent as `Authorization: Bearer <key>`.

### Custom headers

`[request.headers]` replaces the default Authorization header entirely —
spell out every auth header you need. `{{api_key}}` expands wherever it
appears, so the secret stays in `api_key` while the headers control its
shape (e.g. Azure-style `api-key` auth):

```toml
[request.headers]
api-key = "{{api_key}}"
```

### Extra body fields

`[request.body]` merges extra fields into every chat-completions request,
for engine-specific options. `stream`, `model`, `messages`, and `tools`
are owned by the server and rejected here. Most engines only report
token usage on streamed responses when asked, so the example config
ships:

```toml
[request.body]
stream_options = { include_usage = true }
```

### Web tools

`[web_tools]` enables the server-side web tools — the same providers the
hosted service uses. Each configured key enables its tool:
`brave_api_key` enables `web_search` (Brave Search API),
`firecrawl_api_key` enables `web_scrape` (Firecrawl). Keys take the same
three forms as `api_key`. Scraping goes through Firecrawl rather than
fetching model-chosen URLs directly, so requests to arbitrary URLs
originate from Firecrawl's network, not yours.

```toml
[web_tools]
brave_api_key = { env = "BRAVE_API_KEY" }
firecrawl_api_key = { env = "FIRECRAWL_API_KEY" }
```

## Authentication

Set the `AUTH_TOKEN` environment variable to require
`Authorization: Bearer <token>` on every request. Unset means open
access — fine on loopback, not anywhere else.

## Connecting the Atuin CLI

```toml
# ~/.config/atuin/config.toml
[ai]
endpoint = "http://localhost:8080"
# api_token = "..."   # must match AUTH_TOKEN when set
```

## Routes

- `POST /api/cli/chat` — the streaming chat endpoint
- `GET /api/cli/models` — the configured model list

## Limitations

- Chat-completions APIs only. Models that are exclusively served via
  OpenAI's newer `/v1/responses` protocol are not supported.
- Model failures surface in the stream: a misconfigured model name or
  unreachable endpoint fails the request immediately with the upstream's
  status and error message.

## License

Apache-2.0; see [LICENSE](LICENSE). The engine lives in
[atuin-ai-core](https://github.com/atuinsh/atuin-ai-core).
