# cli_chat_standalone

A minimal self-hosted server for the Atuin CLI chat protocol, backed by
any OpenAI-compatible chat-completions endpoint (Ollama, vLLM, LM Studio,
llama.cpp, LiteLLM, ...). The engine is the shared Gleam package in
`../gleam_cli_chat_core` — the same code the hosted deployment serves —
composed here with stateless defaults: no accounts, no limits, no
recording.

## Run

```sh
cp config.example.toml config.toml   # then edit
mix deps.get
mix run --no-halt
```

Environment:

- `CHAT_CONFIG` — path to the config file (default `config.toml`)
- `AUTH_TOKEN` — when set, every request must carry
  `Authorization: Bearer <token>`; unset means open access (loopback use)

Point the Atuin CLI at it with `ai.endpoint = "http://localhost:8080"`.

## Routes

- `POST /api/cli/chat` — the streaming chat endpoint
- `GET /api/cli/models` — the configured model list
