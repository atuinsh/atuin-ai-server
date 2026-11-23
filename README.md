# Atuin AI

Atuin AI is a self-hosted terminal-first AI agent.

> [!NOTE]
>
> Atuin AI is entering low-maintenance mode. Security concerns addressed, however, Atuin Inc. no
> longer provides an official, managed server.

## Quick Start

To get started with Atuin AI, you need to host an instance of the server, as described in the
[following section](#self-hosting).

Then, install Atuin AI by running:

```bash
cargo TODO
```

### Self-Hosting

Atuin no longer provides a managed installation of Atuin AI. **You must host your own instance.**
The easiest way to host this Atuin AI is via our docker image:

```bash
# Copy the server config file into your local config file.
# Please see the comment lines in the example file for more details.
cp server/config.example.toml config.toml

# Run the server in your docker container.
docker run \
  -v ./config.toml:/etc/atuin-ai/config.toml \
  -p 8080:8080 \
  ghcr.io/atuinsh/atuin-ai-server:latest
```

#### Authentication

Set the `AUTH_TOKEN` environment variable to require `Authorization: Bearer <token>` on every
request. Unset means open access. Set the client's `ai.api_token` to match.

## License

The server is Apache-2.0; see [LICENSE](LICENSE). The client is MIT, as the Atuin code it comes
from; see [client/LICENSE](client/LICENSE).
