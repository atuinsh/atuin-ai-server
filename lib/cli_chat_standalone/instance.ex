defmodule CliChatStandalone.Instance do
  @moduledoc """
  Composes the engine `Instance` for this deployment: the stateless
  builder defaults (no limits, no recording, inline tool results, no
  server tools) plus a backend that routes every catalog model to the
  configured OpenAI-compatible endpoint.
  """

  alias CliChatStandalone.Config

  def build(%Config{options_by_model: options_by_model, catalog: catalog}) do
    backend = fn resolved_model, _session_id ->
      case Map.fetch(options_by_model, resolved_model) do
        {:ok, options} -> {:some, {:open_ai_endpoint, options}}
        :error -> :none
      end
    end

    :atuin_hub@cli_chat@instance.new(catalog, backend)
  end
end
