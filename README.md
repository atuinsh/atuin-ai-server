# Atuin AI

Atuin AI is a self-hosted terminal-first AI agent.

> [!NOTE]
>
> Atuin AI is in low-maintenance mode. Security concerns are addressed, however, Atuin Inc. no
> longer provides an official, managed server. You must self-host Atuin AI.

![Atuin AI suggesting a command, then rebuilding a Docker image from shell history](res/demo.gif)

## Quick Start

To get started with Atuin AI, you need to host an instance of the server, as described in the
[following section](#self-hosting).

First, ensure that `atuin` itself is installed. Then, install Atuin AI by running:

```bash
cargo install --locked --git https://github.com/atuinsh/atuin-ai atuin-ai
```

Once installed, configure your shell hooks:

```bash
# ~/.zshrc
eval "$(atuin init zsh)" # <- you will already have this
eval "$(atuin-ai init zsh)"

# ~/.bashrc
eval "$(atuin init bash)" # <- you will already have this
eval "$(atuin-ai init bash)"

# ~/.config/fish/config.fish
atuin init fish | source # <- you will already have this
atuin-ai init fish | source
```

Finally, configure your `atuin-ai` to point at your self-hosted instance. In `~/.atuin/ai.toml`:

```toml
endpoint = "<your-url>"
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
request. Unset means open access. Set `api_token` in the client's `ai.toml` to match.

## License

The server is Apache-2.0; see [LICENSE](LICENSE). The client is MIT, as the Atuin code it comes
from; see [client/LICENSE](client/LICENSE).
