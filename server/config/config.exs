import Config

# Tests boot the server themselves with their own operator config.
config :atuin_ai_server, server: config_env() != :test
