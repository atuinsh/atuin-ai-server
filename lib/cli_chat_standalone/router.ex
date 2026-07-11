defmodule CliChatStandalone.Router do
  @moduledoc """
  The two routes of the standard CLI client protocol, served by the shared
  Gleam engine. Paths match the hosted deployment's, so a client pointed
  at this server via its hub URL works unchanged.
  """

  use Plug.Router

  alias CliChatStandalone.State

  plug(:match)
  plug(:auth)
  plug(Plug.Parsers, parsers: [:json], json_decoder: JSON)
  plug(:dispatch)

  post "/api/cli/chat" do
    %{instance: instance} = State.get()

    # options_enabled: true — the model/run_preference fields are not
    # feature-gated here. llm_selection_enabled: false — free-form model
    # strings resolve through OpenRouter-flavored prefix rules that don't
    # fit a custom endpoint, so only catalog aliases are served.
    env =
      {:request_env, "standalone", current_date(), :metadata_only, true, false}

    :atuin_hub@cli_chat@http@controller.serve(conn, conn.params, instance, env)
  end

  get "/api/cli/models" do
    %{catalog: catalog} = State.get()
    :atuin_hub@cli_chat@http@controller.models_response(conn, catalog, false)
  end

  match _ do
    conn
    |> put_resp_content_type("application/json")
    |> send_resp(404, ~s({"error":"not_found","message":"No such route"}))
  end

  # With AUTH_TOKEN unset the server is open — for local engines on
  # loopback. Anything reachable by others should set it.
  defp auth(conn, _opts) do
    case State.get().auth_token do
      nil ->
        conn

      token ->
        case get_req_header(conn, "authorization") do
          ["Bearer " <> ^token] ->
            conn

          _ ->
            conn
            |> put_resp_content_type("application/json")
            |> send_resp(
              401,
              ~s({"error":"unauthorized","message":"Invalid or missing bearer token"})
            )
            |> halt()
        end
    end
  end

  defp current_date do
    Calendar.strftime(DateTime.utc_now(), "%B %d, %Y")
  end
end
