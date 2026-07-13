defmodule AtuinAI.Server.Application do
  @moduledoc false

  use Application

  alias AtuinAI.Server.Config

  @impl true
  def start(_type, _args) do
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
