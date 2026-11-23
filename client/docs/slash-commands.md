# Slash commands

The agent has slash commands that control its behavior. Type `/` in its prompt, and a list of all available slash commands will appear. You can continue typing to filter the list of commands, and press `tab` to expand the selected command.

## Built-in commands

- **`/help`**: list the slash commands, with a short description of each and a link to these docs.
- **`/new`**: archive the current session and start a fresh one.
- **`/model`**: pick one of the server's models. The choice is saved as `ai.model`, the default for new sessions.

## Commands from skills

Any [user-defined skills](./skills.md) with a name will register that name as a slash command, with the skill's description as the slash command description (shown in `/help` and in the slash command fuzzy picker). For example, if you have a skill named `my-skill`, you can invoke it by typing `/my-skill` in the prompt. If the skill has any parameters, you can provide them after the skill name, separated by spaces.
