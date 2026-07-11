import Config

# Tests boot the server themselves with their own operator config.
config :cli_chat_standalone, server: config_env() != :test
