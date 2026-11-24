# Atuin AI Server

A minimal self-hosted server for the Atuin AI protocol, backed by any
OpenAI-compatible chat-completions endpoint.

The [client](../client) talks to it: set `endpoint` in the client's
`ai.toml` to this server, and chat runs against your own models.

Commands below run from this `server/` directory.

## Quick start (Docker)

```sh
# Copy the server config file into your local config file.
# Please see the comment lines in the example file for more details.
cp config.example.toml config.toml

# Run the server in your docker container.
docker run \
  -v ./config.toml:/etc/atuin-ai/config.toml \
  -p 8080:8080 \
  --add-host host.docker.internal:host-gateway \
  ghcr.io/atuinsh/atuin-ai-server:latest
```

Or build your own image:

```sh
docker build -t atuin-ai-server .
```

The example config reaches Ollama on the Docker host through
`host.docker.internal`. Docker Desktop defines that name, and `--add-host`
defines it on Linux, where it is the Docker bridge rather than loopback. Ollama
only listens on loopback by default, so on Linux start it with
`OLLAMA_HOST=0.0.0.0`, and keep port 11434 off the public network.

## Limitations

- Chat-completions APIs only. Models that are exclusively served via OpenAI's
  newer `/v1/responses` protocol are not supported.
- Model failures surface in the stream: a misconfigured model name or
  unreachable endpoint fails the request immediately with the upstream's status
  and error message in the Atuin AI TUI.

## License

Apache-2.0; see [LICENSE](../LICENSE). The engine lives in
[atuin-ai-core](https://github.com/atuinsh/atuin-ai-core).
