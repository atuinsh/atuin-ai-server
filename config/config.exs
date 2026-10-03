import Config

# Tests boot the server themselves with their own operator config.
config :atuin_ai_server, server: config_env() != :test

# Elixir's default console format starts every entry with a newline,
# leaving a blank line between log lines. LOG_FORMAT=json|ecs replaces the
# formatter entirely at boot (AtuinAI.Server.LogFormat).
config :logger, :default_formatter, format: "$time $metadata[$level] $message\n"
