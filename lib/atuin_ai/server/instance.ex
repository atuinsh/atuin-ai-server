defmodule AtuinAI.Server.Instance do
  @moduledoc """
  Composes the engine `Instance` for this deployment: the stateless
  builder defaults (no limits, no recording, inline tool results) plus a
  backend that routes every catalog model to the configured
  OpenAI-compatible endpoint, and — when `[web_tools]` is configured —
  the same server-side web tools the hosted deployment registers
  (Brave-backed `web_search`, Firecrawl-backed `web_scrape`).
  """

  alias AtuinAI.Server.Config

  def build(%Config{options_by_model: options_by_model, catalog: catalog} = config) do
    backend = fn resolved_model, _session_id ->
      case Map.fetch(options_by_model, resolved_model) do
        {:ok, options} -> {:some, {:open_ai_endpoint, options}}
        :error -> :none
      end
    end

    :atuin_ai_core@instance.new(catalog, backend)
    |> with_web_tools(config.web_tools)
  end

  defp with_web_tools(instance, nil), do: instance

  defp with_web_tools(instance, web_tools) do
    # web_tools.Env — empty string means unconfigured (the engine's
    # convention); the engine's dream-backed transport performs the
    # HTTP calls. Registration order is part of the prompt-cache
    # contract: search before scrape, matching the hosted deployment.
    env =
      {:env, web_tools.brave_api_key || "", web_tools.firecrawl_api_key || "",
       &:atuin_ai_core@http@web_transport.send/1}

    instance
    |> register_if(
      web_tools.brave_api_key,
      :atuin_ai_core@domain@tools@web_search.web_search(),
      fn input -> :atuin_ai_core@http@web_tools.search(env, input) end
    )
    |> register_if(
      web_tools.firecrawl_api_key,
      :atuin_ai_core@domain@tools@web_scrape.web_scrape(),
      fn input -> :atuin_ai_core@http@web_tools.scrape(env, input) end
    )
  end

  defp register_if(instance, nil, _definition, _execute), do: instance

  defp register_if(instance, _key, definition, execute) do
    :atuin_ai_core@instance.with_server_tool(instance, definition, execute)
  end
end
