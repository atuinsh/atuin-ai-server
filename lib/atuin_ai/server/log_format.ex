defmodule AtuinAI.Server.LogFormat do
  @moduledoc """
  Log output format, selected by the `LOG_FORMAT` environment variable:

    * `text` (default) — Elixir's console format, one line per entry
    * `json` — one JSON object per line
    * `ecs` — one Elastic Common Schema (ECS) JSON object per line

  For `json` and `ecs` this module is installed as the formatter of the
  default `:logger` handler, so engine (Gleam/Erlang) and Elixir log
  entries come out the same way.

  The engine's observer logs `"[scope] event key=value ..."` lines. Those
  are lifted into structured fields — scope and event name, plus the
  key/value pairs as strings — while the full text stays in the message.
  A value runs until the next ` key=`, so values with spaces survive.
  """

  @ecs_version "8.11.0"
  @formats %{"text" => :text, "json" => :json, "ecs" => :ecs}

  @doc "Parses a `LOG_FORMAT` value; `nil` means the default, `:text`."
  def parse(nil), do: {:ok, :text}

  def parse(value) do
    case Map.fetch(@formats, value |> String.trim() |> String.downcase()) do
      {:ok, format} -> {:ok, format}
      :error -> {:error, "LOG_FORMAT must be one of text, json, ecs; got: #{inspect(value)}"}
    end
  end

  @doc "Installs the formatter for `value` on the default handler; raises on a bad value."
  def configure!(value) do
    case parse(value) do
      {:ok, :text} ->
        :ok

      {:ok, format} ->
        :ok = :logger.update_handler_config(:default, :formatter, {__MODULE__, %{format: format}})

      {:error, message} ->
        raise message
    end
  end

  # :logger formatter callbacks

  @doc false
  def check_config(%{format: format}) when format in [:json, :ecs], do: :ok
  def check_config(config), do: {:error, {:invalid_formatter_config, config}}

  @doc false
  def format(%{level: level, msg: msg, meta: meta}, %{format: format}) do
    message = message(msg, meta)
    [JSON.encode!(document(format, level, message, meta, parse_engine_line(message))), ?\n]
  catch
    kind, reason -> [JSON.encode!(fallback(format, level, kind, reason, msg, meta)), ?\n]
  end

  # Formatting raised or threw (e.g. a broken report_cb): still one JSON
  # object per line, with every field that doesn't depend on the message,
  # plus the failure and the raw message. Only the engine fields are lost.
  defp fallback(format, level, kind, reason, msg, meta) do
    error_key = if format == :ecs, do: "error.message", else: "error"

    format
    |> document(level, inspect(msg), meta, nil)
    |> Map.put(error_key, Exception.format_banner(kind, reason))
  end

  defp document(:json, level, message, meta, engine) do
    %{"time" => timestamp(meta), "level" => to_string(level), "message" => message}
    |> put_if("logger", logger(meta))
    |> put_engine(engine, "scope", "event", "fields")
  end

  defp document(:ecs, level, message, meta, engine) do
    %{
      "@timestamp" => timestamp(meta),
      "log.level" => to_string(level),
      "message" => message,
      "ecs.version" => @ecs_version,
      "service.name" => "atuin-ai-server"
    }
    |> put_if("log.logger", logger(meta))
    |> put_if("log.origin.function", function(meta))
    |> put_if("log.origin.file.line", line(meta))
    |> put_engine(engine, "event.dataset", "event.action", "labels")
  end

  defp message({:string, chardata}, _meta), do: chardata |> IO.chardata_to_string() |> printable()

  defp message({:report, report}, meta) do
    case meta do
      %{report_cb: callback} when is_function(callback, 1) ->
        {format, args} = callback.(report)
        message({format, args}, %{})

      %{report_cb: callback} when is_function(callback, 2) ->
        report
        |> callback.(%{depth: :unlimited, chars_limit: :unlimited, single_line: false})
        |> IO.chardata_to_string()
        |> printable()

      _ ->
        inspect(report)
    end
  end

  defp message({format, args}, _meta),
    do: format |> :io_lib.format(args) |> IO.chardata_to_string() |> printable()

  defp printable(string) do
    string = String.trim_trailing(string)
    if String.valid?(string), do: string, else: inspect(string)
  end

  defp timestamp(%{time: time}) when is_integer(time),
    do: time |> :calendar.system_time_to_rfc3339(unit: :microsecond, offset: ~c"Z") |> to_string()

  defp timestamp(_), do: DateTime.utc_now() |> DateTime.to_iso8601()

  defp logger(%{mfa: {module, _, _}}), do: inspect(module)
  defp logger(_), do: nil

  defp function(%{mfa: {_, name, arity}}) when is_atom(name) and is_integer(arity),
    do: "#{name}/#{arity}"

  defp function(_), do: nil

  defp line(%{line: line}) when is_integer(line), do: line
  defp line(_), do: nil

  defp put_if(doc, _key, nil), do: doc
  defp put_if(doc, key, value), do: Map.put(doc, key, value)

  defp put_engine(doc, nil, _scope_key, _event_key, _fields_key), do: doc

  defp put_engine(doc, {scope, event, fields}, scope_key, event_key, fields_key) do
    doc
    |> Map.merge(%{scope_key => scope, event_key => event})
    |> then(&if(fields == %{}, do: &1, else: Map.put(&1, fields_key, fields)))
  end

  @doc false
  # "[cli_chat] llm_call_failed session_id=a detail=connection refused duration_ms=1"
  # → {"cli_chat", "llm_call_failed", %{"session_id" => "a", "detail" => "connection refused", ...}}
  # The event runs up to the first key=; a multi-word one ("[cli_chat] turn
  # failed ...") is snake_cased to match the rest ("turn_failed").
  def parse_engine_line(message) do
    case Regex.run(
           ~r/\A\[([a-z0-9_]+)\] ([a-z0-9_]+(?: [a-z0-9_]+)*?)(?: ([a-z0-9_]+=.*))?\z/s,
           message
         ) do
      [_, scope, event] -> {scope, event_name(event), %{}}
      [_, scope, event, rest] -> {scope, event_name(event), fields(rest)}
      nil -> nil
    end
  end

  defp event_name(words), do: String.replace(words, " ", "_")

  defp fields(rest) do
    rest
    |> String.split(~r/ (?=[a-z0-9_]+=)/)
    |> Enum.reduce(%{}, fn pair, acc ->
      case String.split(pair, "=", parts: 2) do
        [key, value] -> Map.put(acc, key, value)
        _ -> acc
      end
    end)
  end
end
