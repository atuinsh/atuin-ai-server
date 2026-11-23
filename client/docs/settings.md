# Settings

The agent's settings are the `[ai]` table of Atuin's `config.toml`. Like Atuin, an `ATUIN_AI__<KEY>`
environment variable overrides a key, e.g. `ATUIN_AI__MODEL=llama`.

The agent also reads a few of Atuin's own settings: `data_dir`, `db_path` (the history database the
`atuin_history` tool searches), `local_timeout`, and `[logs]` (`enabled`, `dir`, `level`,
`retention`).

### `endpoint`

Default: unset, which is an error

The Atuin AI server to chat with, such as `http://localhost:8080`. A path prefix is kept, so
`https://example.com/ai` sends requests to `https://example.com/ai/api/cli/chat`.

### `api_token`

Default: unset

Sent as `Authorization: Bearer <token>`. Set it to the server's `AUTH_TOKEN`. Unset or empty
sends no token.

### `enabled`

Default: unset

`false` turns the agent off: `atuin-ai init` binds nothing, and `atuin-ai inline` exits at once.

### `model`

Default: unset

The server's model alias to request for new sessions; unset uses the server's default. `/model`
lists the server's models, and saves the one you choose here.

### `session_continue_minutes`

Default: `1h`

How long after your last message a conversation in the same directory or repository is picked
up again, as a duration such as `30m` or `2h`, or a bare number of minutes. `0` always starts a
new session. `/new` starts one by hand.

### `db_path`

Default: `ai_sessions.db` in Atuin's `data_dir`

The agent's conversation database. A relative path is relative to `config.toml`.

### `yolo`

Default: `false`

Allow every tool call without asking. **Use with caution.** It enables no capabilities: it only
skips the permission checks.

### `tips`

Default: `true`

Show tips at the bottom of agent turns.

## Capabilities

What the agent tells the server it can do, under `[ai.capabilities]`. A disabled capability's
tools are never offered to the model.

### `enable_history_search`

Default: `true`

The `atuin_history` tool, which searches your Atuin history. It is also off when the history
database can't be opened.

### `enable_file_tools`

Default: `true`

The `read_file`, `edit_file` and `write_file` tools.

### `enable_command_execution`

Default: `true`

The `execute_shell_command` tool.

```toml
[ai.capabilities]
enable_history_search = false
```

## Opening context

What the first request of a session tells the server, under `[ai.opening]`. By default, only your
OS and shell are sent.

### `send_cwd`

Default: `false`

Send your working directory. `[ai] send_cwd` is the deprecated spelling.

### `send_last_command`

Default: `false`

Send the last command in your Atuin history.

```toml
[ai.opening]
send_cwd = true
send_last_command = true
```
