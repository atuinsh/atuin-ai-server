# Tools and permissions

The agent has tools it can use on your system, with your permission, to answer questions and act
on your behalf. Which ones the model is offered is set by [capabilities](../ai.example.toml).

## Permissions

By default, the agent asks before every tool call. A _permission file_ changes that.

### Permission files

Permission files live at `.atuin/permissions.ai.toml` in any project. When the model asks for a
tool, the agent checks for a `.atuin/permissions.ai.toml` in its working directory and in every
parent directory up to the root, and finally `permissions.ai.toml` in Atuin's home directory
(`~/.atuin/permissions.ai.toml` by default).

Because that is the global file, "Always allow in this directory" is unavailable when your
current directory is `$HOME`. Likewise, when `$HOME` is a repository, "Always allow in this
workspace" is unavailable in it. Pick "Always allow" if a global rule is acceptable.

```toml
[permissions]

allow = [
    # rules for tools allowed without asking
]

deny = [
    # rules for tools denied without asking
]

ask = [
    # rules for tools that always ask
]
```

With no matching rule, the agent asks.

A file deeper in the filesystem takes priority over one higher up: a rule allowing a tool in the
working directory wins over a parent directory's rule denying it. Within a file, `ask` rules take
priority over `deny` rules, which take priority over `allow` rules.

### Scopes

Most rules can be scoped. For file tools, the scope is a glob matched against file paths.

This file lets the agent read and write any Markdown file in the project (`Write` implies `Read`,
see below), and denies it `.env`. Any other file asks first.

```toml
[permissions]

allow = [
    "Write(**/*.md)"
]

deny = [
    "Read(.env)"
]
```

## Tools

### Atuin history

`atuin_history` searches your Atuin history, read-only. The model reaches for it when you ask
about a command you ran before, or for help with one that failed ("why did my last command
fail?").

Each query term matches as a substring first, then fuzzily (its letters in order), with smart
case: a term with an uppercase letter matches case-sensitively.

**Rule:** `AtuinHistory`

**Capability:** `capabilities.enable_history_search`

```toml
[permissions]
allow = ["AtuinHistory"]
```

### Read

`read_file` reads files, when you ask about a file's contents, ask for edits to it, or ask a
question a file answers best.

**Rule:** `Read(<glob>)`, for example `Read(**/*.md)` for every Markdown file under the current
directory. `Read` alone matches every file.

**Capability:** `capabilities.enable_file_tools`, which covers `Read` and `Write`.

```toml
[permissions]
allow = ["Read(**/*.md)"]
deny = ["Read(.secret/**)"]
```

> [!WARNING]
> **Write implies Read.** The agent must read a file before writing it, so it doesn't lose data
> it hasn't seen. A rule allowing `Write` for some files also allows `Read` for them: with
> `Write(**/*.md)`, the agent can read any Markdown file under the current directory, with no
> `Read(**/*.md)` rule.

### Write

`edit_file` and `write_file` create and edit files, for example to update a tool's
configuration or to fix a problem you're debugging.

**Rule:** `Write(<glob>)`, for example `Write(**/*.md)`. `Write` alone matches every file.

**Capability:** `capabilities.enable_file_tools`, which covers `Read` and `Write`.

```toml
[permissions]
allow = ["Write(**/*.md)"]
deny = ["Write(.secret/**)"]
```

> [!NOTE]
> **Backups.** The first time the agent writes to a file in a session, it backs up the original
> under `ai/snapshots/<session_id>/` in Atuin's data directory, beside a `manifest.json` mapping
> each backup to the file it came from.

### Shell commands

`execute_shell_command` runs shell commands: when an action is best done by running a command,
when you ask for help debugging one, or during a multi-step task.

**Rule:** `Shell(<pattern>)`, for example `Shell(git *)` for any command starting with `git`.
`Shell` alone matches every command.

**Capability:** `capabilities.enable_command_execution`

```toml
[permissions]
allow = [
    "Shell(git add *)",
    "Shell(git commit *)"
]
```

A pattern is matched against the command's words, and `*` behaves differently depending on
where it is:

| Pattern | Matches | Doesn't match |
|---------|---------|---------------|
| `*` | any command | |
| `git commit *` | `git commit`, `git commit -m "msg"` | `git`, `git push` |
| `ls*` | `ls`, `ls -a`, `lsof` | `cat` |
| `git * --amend` | `git commit --amend`, `git rebase --amend` | `git commit` |
| `git commit` | `git commit` | `git`, `git push`, `git commit -m "msg"` |

`ls *` (with a space) matches at word boundaries: `ls` and `ls -a`, but not `lsof`. `ls*` (no
space) matches by prefix, `lsof` included.

In `allow` and `ask` rules, a pattern without a wildcard is an **exact match**: `git commit`
matches only `git commit`, so allow `git commit *` for any arguments. In `deny` rules, a pattern
without a wildcard is a **prefix match**: `rm` denies `rm`, `rm -rf /` and `rm ./README.md`
alike.

> [!WARNING]
> **Compound commands.** A compound command such as `git add . && npm test` is split into its
> subcommands, and runs without asking only when every subcommand is allowed: here, by both
> `Shell(git add *)` and `Shell(npm test)`. The splitting isn't perfect, and some shells parse
> worse than others, so be careful allowing broad patterns.
>
> Commands nested in others, as in `$(..)`, are checked too. A command the parser can't read all
> the way through, such as one with a syntax error, asks unless `Shell` alone allows everything.

### Load skill

`load_skill` loads one of your [skills](skills.md) when the model finds it relevant. A skill that
embeds commands runs them as it loads, so loading it asks first; any other skill loads without
asking.

**Rule:** `LoadSkill`

```toml
[permissions]
allow = ["LoadSkill"]
```
