defmodule AtuinAI.Server.Application do
  @moduledoc false

  use Application

  alias AtuinAI.Server.Config

  @impl true
  def start(_type, _args) do
    # The engine's apps are code-only (`:load`) in the boot script, so
    # nothing starts them for us. Since 5.1.3, dream_http_client is a
    # real OTP application whose start callback creates the ETS tables
    # the streaming machinery needs — without this, streams crash on
    # first use.
    {:ok, _} = Application.ensure_all_started(:dream_http_client)

    children =
      if Application.get_env(:atuin_ai_server, :server, true) do
        config = Config.load!(config_path())
        AtuinAI.Server.State.put(config, auth_token())
        [{Bandit, plug: AtuinAI.Server.Router, port: config.port}]
      else
        # Tests boot the server themselves with their own config.
        []
      end

    Supervisor.start_link(children, strategy: :one_for_one, name: AtuinAI.Server.Supervisor)
  end

  defp config_path do
    System.get_env("CHAT_CONFIG", "config.toml")
  end

  defp auth_token do
    System.get_env("AUTH_TOKEN")
  end
end
