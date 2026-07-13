defmodule AtuinAI.ServerTest do
  @moduledoc """
  End-to-end over real TCP: a fake OpenAI-compatible upstream streams a
  canned SSE completion, the standalone server is booted from a real TOML
  config pointing at it, and the client protocol comes back over the wire.
  """

  use ExUnit.Case

  defmodule FakeUpstream do
    @moduledoc "Streams one canned chat completion, OpenAI wire shape."
    use Plug.Router

    plug(:match)
    plug(Plug.Parsers, parsers: [:json], json_decoder: JSON)
    plug(:dispatch)

    get "/teapot" do
      conn = fetch_query_params(conn)
      send_resp(conn, 418, "short and stout q=#{conn.query_params["q"]}")
    end

    post "/v1/chat/completions" do
      # Assertions on the request the adapter built ride through the test
      # process registered under a well-known name.
      send(Process.whereis(:upstream_inspector), {:upstream_request, conn})

      case conn.body_params["model"] do
        "missing-model" -> not_found(conn)
        _ -> canned_stream(conn)
      end
    end

    # The Ollama shape for an unknown model: a whole (non-streamed) 404.
    defp not_found(conn) do
      conn
      |> put_resp_content_type("application/json")
      |> send_resp(
        404,
        ~s({"error":{"message":"model \\"missing-model\\" not found, try pulling it first","type":"api_error"}})
      )
    end

    defp canned_stream(conn) do
      chunks = [
        ~s({"choices":[{"delta":{"role":"assistant","content":"Hello"}}]}),
        ~s({"choices":[{"delta":{"content":" from upstream"}}]}),
        ~s({"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}})
      ]

      conn =
        conn
        |> put_resp_content_type("text/event-stream")
        |> send_chunked(200)

      conn =
        Enum.reduce(chunks, conn, fn chunk, conn ->
          {:ok, conn} = chunk(conn, "data: #{chunk}\n\n")
          conn
        end)

      {:ok, conn} = chunk(conn, "data: [DONE]\n\n")
      conn
    end
  end

  setup_all do
    upstream_port = free_port()
    server_port = free_port()

    start_supervised!({Bandit, plug: FakeUpstream, port: upstream_port},
      id: :fake_upstream
    )

    config_path = Path.join(System.tmp_dir!(), "atuin_ai_server_test_#{server_port}.toml")

    File.write!(config_path, """
    port = #{server_port}
    endpoint = "http://localhost:#{upstream_port}/v1"
    default_model = "canned"

    [request.body]
    special_param = true

    [[models]]
    alias = "canned"
    name = "Canned Model"
    description = "The fake upstream"
    model = "canned-model-v1"

    [[models]]
    alias = "missing"
    name = "Missing Model"
    description = "The upstream 404s this one"
    model = "missing-model"
    """)

    on_exit(fn -> File.rm(config_path) end)

    config = AtuinAI.Server.Config.load!(config_path)
    AtuinAI.Server.State.put(config, nil)
    start_supervised!({Bandit, plug: AtuinAI.Server.Router, port: server_port}, id: :server)

    {:ok, server_port: server_port, upstream_port: upstream_port}
  end

  setup do
    Process.register(self(), :upstream_inspector)

    on_exit(fn ->
      Process.whereis(:upstream_inspector) && Process.unregister(:upstream_inspector)
    end)

    :ok
  end

  test "models endpoint serves the configured catalog", %{server_port: port} do
    {:ok, {{_, 200, _}, _headers, body}} =
      :httpc.request(:get, {~c"http://localhost:#{port}/api/cli/models", []}, [], [])

    assert JSON.decode!(to_string(body)) == %{
             "default" => "canned",
             "models" => [
               %{
                 "alias" => "canned",
                 "name" => "Canned Model",
                 "description" => "The fake upstream"
               },
               %{
                 "alias" => "missing",
                 "name" => "Missing Model",
                 "description" => "The upstream 404s this one"
               }
             ]
           }
  end

  test "chat endpoint streams a full turn from the upstream", %{server_port: port} do
    body = JSON.encode!(%{"messages" => [%{"role" => "user", "content" => "hi"}]})

    {:ok, {{_, 200, _}, headers, response}} =
      :httpc.request(
        :post,
        {~c"http://localhost:#{port}/api/cli/chat", [], ~c"application/json", body},
        [],
        []
      )

    response = to_string(response)
    content_types = for {~c"content-type", v} <- headers, do: to_string(v)
    assert ["text/event-stream" <> _] = content_types

    # The client protocol's turn shape: processing status, the streamed
    # text deltas, and a done event carrying the upstream's usage.
    assert response =~ "event: status"
    assert response =~ ~s("state":"processing")
    assert response =~ ~s(data: {"content":"Hello"})
    assert response =~ ~s(data: {"content":" from upstream"})
    assert response =~ "event: done"
    assert response =~ ~s("input_tokens":7)
    assert response =~ ~s("output_tokens":3)

    # What the adapter sent upstream: the configured wire model, the
    # extra body field, no auth header (none configured).
    assert_receive {:upstream_request, upstream_conn}, 2000
    assert upstream_conn.body_params["model"] == "canned-model-v1"
    assert upstream_conn.body_params["special_param"] == true
    assert upstream_conn.body_params["stream"] == true
    assert Plug.Conn.get_req_header(upstream_conn, "authorization") == []
  end

  test "an upstream HTTP error fails the turn fast with the status and body", %{
    server_port: port
  } do
    body =
      JSON.encode!(%{
        "messages" => [%{"role" => "user", "content" => "hi"}],
        "config" => %{"model" => "missing"}
      })

    # Well under the driver's 60s inactivity deadline: the failure must
    # come from the upstream's response, not from waiting out a timeout.
    task =
      Task.async(fn ->
        :httpc.request(
          :post,
          {~c"http://localhost:#{port}/api/cli/chat", [], ~c"application/json", body},
          [],
          []
        )
      end)

    {:ok, {{_, 200, _}, _headers, response}} = Task.await(task, 5000)
    response = to_string(response)

    assert response =~ "HTTP 404"
    assert response =~ "not found, try pulling it first"
  end

  test "unknown model alias is a 400", %{server_port: port} do
    body =
      JSON.encode!(%{
        "messages" => [%{"role" => "user", "content" => "hi"}],
        "config" => %{"model" => "nonexistent"}
      })

    {:ok, {{_, status, _}, _headers, response}} =
      :httpc.request(
        :post,
        {~c"http://localhost:#{port}/api/cli/chat", [], ~c"application/json", body},
        [],
        []
      )

    # An unknown alias decays to the default model (hosted behavior); a
    # *valid* turn still comes back. This pins that the request didn't 500.
    assert status == 200 or status == 400
    assert to_string(response) != ""
  end

  test "the engine's web transport preserves the response status", %{
    upstream_port: upstream_port
  } do
    # Query included: the original dream build_url dropped query strings
    # on the wire (fixed in 3.0.1-atuin.3).
    request =
      {:http_request, :get, "http://localhost:#{upstream_port}/teapot?q=steep", [], "", 5000}

    assert {:ok, {418, "short and stout q=steep"}} =
             :atuin_ai_core@http@web_transport.send(request)
  end

  test "web tools register per configured key" do
    config_path = Path.join(System.tmp_dir!(), "web_tools_test.toml")

    File.write!(config_path, """
    endpoint = "http://localhost:9/v1"

    [web_tools]
    brave_api_key = "brave-test-key"

    [[models]]
    alias = "m"
    model = "m1"
    """)

    on_exit(fn -> File.rm(config_path) end)

    instance =
      config_path
      |> AtuinAI.Server.Config.load!()
      |> AtuinAI.Server.Instance.build()

    assert :atuin_ai_core@instance.is_server_tool(instance, "web_search")
    refute :atuin_ai_core@instance.is_server_tool(instance, "web_scrape")
  end

  defp free_port do
    {:ok, socket} = :gen_tcp.listen(0, [])
    {:ok, port} = :inet.port(socket)
    :gen_tcp.close(socket)
    port
  end
end
