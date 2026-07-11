defmodule CliChatStandalone.State do
  @moduledoc """
  The boot-time state the router reads per request: the composed engine
  `Instance`, the catalog, and the optional bearer token. Persistent-term
  because it's written once at boot and read on every request.
  """

  alias CliChatStandalone.Config

  @key {__MODULE__, :state}

  def put(%Config{} = config, auth_token) do
    :persistent_term.put(@key, %{
      instance: CliChatStandalone.Instance.build(config),
      catalog: config.catalog,
      auth_token: auth_token
    })
  end

  def get, do: :persistent_term.get(@key)
end
