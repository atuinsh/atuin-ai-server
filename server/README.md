# Atuin AI Server

A minimal self-hosted server for the Atuin AI protocol, backed by any
OpenAI-compatible chat-completions endpoint.

The [client](../client) talks to it: point `ai.endpoint` at this server
and chat runs against your own models.

Commands below run from this `server/` directory.

## Quick start (Docker)

```sh
# Copy the server config file into your local config file.
# Please see the comment lines in the example file for more details.
cp server/config.example.toml config.toml

# Run the server in your docker container.
docker run \
  -v ./config.toml:/etc/atuin-ai/config.toml \
  -p 8080:8080 \
  ghcr.io/atuinsh/atuin-ai-server:latest
```

Or build your own image:

```sh
docker build -t atuin-ai-server .
```

To reach an engine running on the Docker host (e.g. local Ollama), use
`host.docker.internal` as the server's endpoint:

```toml
endpoint = "http://host.docker.internal:11434/v1"
```

## Limitations

- Chat-completions APIs only. Models that are exclusively served via OpenAI's
  newer `/v1/responses` protocol are not supported.
- Model failures surface in the stream: a misconfigured model name or
  unreachable endpoint fails the request immediately with the upstream's status
  and error message in the Atuin AI TUI.

## License

Apache-2.0; see [LICENSE](../LICENSE). The engine lives in
[atuin-ai-core](https://github.com/atuinsh/atuin-ai-core).
