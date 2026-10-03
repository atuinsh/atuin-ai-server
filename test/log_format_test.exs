defmodule AtuinAI.Server.LogFormatTest do
  use ExUnit.Case, async: true

  alias AtuinAI.Server.LogFormat

  @time 1_791_063_475_305_077

  defp event(msg, meta \\ %{}) do
    %{
      level: :info,
      msg: msg,
      meta: Map.merge(%{time: @time, mfa: {AtuinAI.Server.Router, :do_match, 4}, line: 19}, meta)
    }
  end

  defp render(event, format) do
    line = event |> LogFormat.format(%{format: format}) |> IO.iodata_to_binary()
    assert String.ends_with?(line, "\n")
    refute line |> String.trim_trailing("\n") |> String.contains?("\n")
    JSON.decode!(line)
  end

  describe "parse/1" do
    test "defaults to text" do
      assert LogFormat.parse(nil) == {:ok, :text}
    end

    test "accepts the three formats, case-insensitively" do
      assert LogFormat.parse("text") == {:ok, :text}
      assert LogFormat.parse("JSON") == {:ok, :json}
      assert LogFormat.parse(" ecs ") == {:ok, :ecs}
    end

    test "rejects anything else" do
      assert {:error, message} = LogFormat.parse("xml")
      assert message =~ "LOG_FORMAT"
    end
  end

  describe "ecs" do
    test "core fields" do
      doc = render(event({:string, "Received request: /api/cli/chat"}), :ecs)

      assert doc["@timestamp"] == "2026-10-03T21:37:55.305077Z"
      assert doc["log.level"] == "info"
      assert doc["message"] == "Received request: /api/cli/chat"
      assert doc["ecs.version"] == "8.11.0"
      assert doc["service.name"] == "atuin-ai-server"
      assert doc["log.logger"] == "AtuinAI.Server.Router"
      assert doc["log.origin.function"] == "do_match/4"
      assert doc["log.origin.file.line"] == 19
      refute Map.has_key?(doc, "labels")
    end

    test "engine lines become event fields and labels" do
      message =
        "[cli_chat] llm_call_failed session_id=s1 trace_id=t1 model=llama " <>
          "error_type=upstream_error detail=connection refused duration_ms=2"

      doc = render(event({:string, message}), :ecs)

      assert doc["message"] == message
      assert doc["event.dataset"] == "cli_chat"
      assert doc["event.action"] == "llm_call_failed"

      assert doc["labels"] == %{
               "session_id" => "s1",
               "trace_id" => "t1",
               "model" => "llama",
               "error_type" => "upstream_error",
               "detail" => "connection refused",
               "duration_ms" => "2"
             }
    end

    test "quotes and newlines stay inside one JSON line" do
      doc = render(event({:string, ~s(bad "quote"\nnext line)}), :ecs)
      assert doc["message"] == ~s(bad "quote"\nnext line)
    end

    test "format strings and reports" do
      assert render(event({~c"~ts", ["formatted"]}), :ecs)["message"] == "formatted"

      report = event({:report, %{a: 1}}, %{report_cb: fn r -> {~c"report ~p", [r]} end})
      assert render(report, :ecs)["message"] == "report \#{a => 1}"

      report = event({:report, %{a: 1}}, %{report_cb: fn r, _config -> ~c"two-arg #{r.a}" end})
      assert render(report, :ecs)["message"] == "two-arg 1"
    end

    test "a failing report_cb still yields one JSON line" do
      broken = event({:report, %{a: 1}}, %{report_cb: fn _ -> throw(:boom) end})

      doc = render(broken, :ecs)
      assert doc["@timestamp"] == "2026-10-03T21:37:55.305077Z"
      assert doc["service.name"] == "atuin-ai-server"
      assert doc["log.logger"] == "AtuinAI.Server.Router"
      assert doc["log.origin.function"] == "do_match/4"
      assert doc["log.origin.file.line"] == 19
      assert doc["log.level"] == "info"
      assert doc["error.message"] =~ "boom"
      assert doc["message"] =~ "report"

      doc = render(broken, :json)
      assert doc["time"] == "2026-10-03T21:37:55.305077Z"
      assert doc["logger"] == "AtuinAI.Server.Router"
      assert doc["level"] == "info"
      assert doc["error"] =~ "boom"
    end
  end

  describe "json" do
    test "plain field names" do
      doc =
        render(event({:string, "[cli_chat] turn_completed outcome=success llm_calls=1"}), :json)

      assert doc["time"] == "2026-10-03T21:37:55.305077Z"
      assert doc["logger"] == "AtuinAI.Server.Router"
      assert doc["level"] == "info"
      assert doc["logger"] == "AtuinAI.Server.Router"
      assert doc["scope"] == "cli_chat"
      assert doc["event"] == "turn_completed"
      assert doc["fields"] == %{"outcome" => "success", "llm_calls" => "1"}
    end
  end

  test "engine line parsing" do
    assert LogFormat.parse_engine_line("[web] started") == {"web", "started", %{}}

    assert LogFormat.parse_engine_line("[cli_chat] turn failed session_id=a detail=boom bang") ==
             {"cli_chat", "turn_failed", %{"session_id" => "a", "detail" => "boom bang"}}

    assert LogFormat.parse_engine_line("[cli_chat] turn failed") ==
             {"cli_chat", "turn_failed", %{}}

    assert LogFormat.parse_engine_line("Starting LLM loop") == nil
  end
end
