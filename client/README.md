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
2. Point the client at it in `~/.atuin/ai.toml`. [`ai.example.toml`](ai.example.toml) has every
   setting:

   ```toml
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

Permission files and skills are the same ones, read from the same places. So are conversations,
in `ai_sessions.db` in Atuin's data directory, unless Atuin's `config.toml` moved them with
`data_dir` or `[ai].db_path`: then move `ai_sessions.db`, and the `ai/snapshots/` backups beside it,
to `~/.atuin/data/`. What changed:

- **Settings live in `ai.toml`.** Move the keys of the `[ai]` table in Atuin's `config.toml` to
  `~/.atuin/ai.toml`, without the `[ai]` header: `[ai.opening]` becomes `[opening]`, and
  `[ai.capabilities]` becomes `[capabilities]`. If `config.toml` moves the history database with
  `data_dir` or `db_path`, set `history_db_path` in `ai.toml` too. See
  [`ai.example.toml`](ai.example.toml).
- **No Atuin Hub.** There's no login and no credit meter. `endpoint` is required, `api_token` is
  the only authentication, and `endpoint_protocol` is ignored.
- **No `atuin_output` tool.** Reading captured command output needs Atuin's daemon, which the
  client doesn't link against, so `capabilities.enable_history_output` is ignored.
- **Simpler history search.** `atuin_history` matches each term as a substring, then fuzzily, with
  Atuin's smart case. Atuin's search operators (`^`, `'`, `!`, `r/.../`) aren't supported.
- **No first-run prompt.** Evaluating `atuin-ai init` is the opt-in, so `enabled` is ignored.
- **Its own log file**, `atuin-ai.log` in Atuin's logs directory, kept for 4 days. `[logs]` is
  ignored; `ATUIN_LOG` sets the level, e.g. `ATUIN_LOG=debug`.

## Docs

- [Settings](ai.example.toml)
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
