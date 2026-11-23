# Atuin AI client

`atuin-ai` is the shell agent Atuin shipped up to 18.24, which
[atuinsh/atuin#4764](https://github.com/atuinsh/atuin/pull/4764) removed in 18.25. It lives on
here as a standalone binary. Press `?` at an empty prompt to chat: it suggests commands and
answers questions. With your permission, it can also search your Atuin history, read and edit
files, and run commands. It talks to an Atuin AI server, such as the one in [`../server`](../server).

## Install

Needs Rust 1.95 or newer, and Atuin: the client reads Atuin's history, and its bash widget is
built on Atuin's.

```sh
cargo install --locked --path client
```

## Set up

1. Run a server: see [`../server`](../server).
2. Point the client at it in Atuin's `config.toml`, the same file Atuin reads (`~/.atuin/` or
   `~/.config/atuin/`):

   ```toml
   [ai]
   endpoint = "http://localhost:8080"
   # api_token = "..."   # when the server sets AUTH_TOKEN
   ```

3. Bind `?` in your shell, after Atuin's own init:

   ```sh
   # ~/.zshrc
   eval "$(atuin init zsh)"
   eval "$(atuin-ai init zsh)"

   # ~/.bashrc
   eval "$(atuin init bash)"
   eval "$(atuin-ai init bash)"

   # ~/.config/fish/config.fish
   atuin init fish | source
   atuin-ai init fish | source
   ```

Press `?` at an empty prompt to start. On a suggested command, Enter runs it and Tab puts it on
the prompt for editing. `atuin-ai inline "question"` starts a chat without the widget.

## Coming from Atuin's built-in agent

The `[ai]` settings, conversations (`ai_sessions.db`), permission files and skills are the same
ones, read from the same places, so nothing needs moving. What changed:

- **No Atuin Hub.** There's no login and no credit meter. `ai.endpoint` is required, `ai.api_token`
  is the only authentication, and `ai.endpoint_protocol` is ignored.
- **No `atuin_output` tool.** Reading captured command output needs Atuin's daemon, which the
  client doesn't link against, so `ai.capabilities.enable_history_output` is ignored.
- **Simpler history search.** `atuin_history` matches each term as a substring, then fuzzily, with
  Atuin's smart case. Atuin's search operators (`^`, `'`, `!`, `r/.../`) aren't supported.
- **No first-run prompt.** Evaluating `atuin-ai init` is the opt-in. `ai.enabled = false` still
  turns the agent off.
- **Its own log file**, `atuin-ai.log` in Atuin's logs directory. `[logs.ai]` stays Atuin's, for
  Threads.

## Docs

- [Settings](docs/settings.md)
- [Tools and permissions](docs/tools-permissions.md)
- [Skills](docs/skills.md)
- [Slash commands](docs/slash-commands.md)

## Development

```sh
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo +nightly fmt   # .rustfmt.toml uses unstable options
```

## License

MIT, as Atuin, which this code comes from; see [LICENSE](LICENSE).
