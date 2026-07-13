defmodule AtuinAI.Server.Config do
  @moduledoc """
  Loads and validates the operator config file (TOML) into everything the
  server needs at boot: the model catalog and the per-model LLM options.

  The file shape:

      port = 8080                       # optional, default 8080
      endpoint = "http://localhost:11434/v1"
      default_model = "llama"           # optional, defaults to the first model

      [api_key]                         # optional; no Authorization header without it
      env = "CHAT_API_KEY"

      [request.headers]                 # optional; replaces the default
      Authorization = "Bearer {{api_key}}"

      [request.body]                    # optional; extra chat-completions body fields

      [[models]]
      alias = "llama"
      name = "Llama 3.3"
      description = "Local llama3.3 via Ollama"
      model = "llama3.3"                # sent as the body's `model`

  URL/header/body validation lives in the Gleam engine
  (`openai_endpoint.options`); this module only shapes the TOML into its
  arguments and turns validation failures into boot errors.
  """

  defstruct [:port, :catalog, :options_by_model, :default_alias]

  def load!(path) do
    raw =
      case Toml.decode_file(path) do
        {:ok, raw} -> raw
        {:error, reason} -> raise "Failed to read config #{path}: #{inspect(reason)}"
      end

    endpoint = fetch!(raw, "endpoint")
    api_key = resolve_api_key(raw["api_key"])
    headers = parse_headers(get_in(raw, ["request", "headers"]))
    extra_body = parse_extra_body(get_in(raw, ["request", "body"]))
    models = parse_models(raw["models"])
    default_alias = default_alias(raw["default_model"], models)

    %__MODULE__{
      port: raw["port"] || 8080,
      catalog: catalog(models, default_alias),
      options_by_model: options_by_model(models, endpoint, api_key, headers, extra_body),
      default_alias: default_alias
    }
  end

  defp fetch!(raw, key) do
    case raw[key] do
      nil -> raise "Config is missing required key \"#{key}\""
      value -> value
    end
  end

  # [api_key] env = "VAR" — the secret stays in the environment; the
  # config file only names it.
  defp resolve_api_key(nil), do: :none

  defp resolve_api_key(key) when is_binary(key) do
    case String.trim(key) do
      "" -> raise "Config api_key must not be empty"
      key -> {:some, key}
    end
  end

  defp resolve_api_key(%{"env" => var}) do
    case System.get_env(var) do
      nil -> raise "Config names api_key env var #{var}, but it is not set"
      value -> {:some, value}
    end
  end

  defp resolve_api_key(%{"cmd" => cmd}) do
    bin = Map.get(cmd, "run", nil)
    args = Map.get(cmd, "args", [])

    if is_nil(bin) or String.length(bin) == 0 do
      raise "Config api_key specified `cmd` with no `run` option"
    end

    case System.cmd(bin, args) do
      {value, 0} ->
        key = String.trim(value)

        if key == "" do
          raise "Config api_key cmd produced an empty value"
        end

        {:some, key}

      {_value, exit} ->
        raise "Config api_key cmd failed with exit #{exit}"
    end
  end

  defp resolve_api_key(other) do
    raise "Config [api_key] must have an `env` or `cmd` key or be a string, got: #{inspect(other)}"
  end

  defp parse_headers(nil), do: :none

  defp parse_headers(%{} = headers) do
    {:some, headers |> Enum.map(fn {name, value} -> {name, to_string(value)} end) |> Enum.sort()}
  end

  defp parse_extra_body(nil), do: []

  defp parse_extra_body(%{} = body) do
    Enum.map(body, fn {key, value} -> {key, to_gleam_json(value)} end)
  end

  defp to_gleam_json(value) when is_boolean(value), do: :gleam@json.bool(value)
  defp to_gleam_json(value) when is_integer(value), do: :gleam@json.int(value)
  defp to_gleam_json(value) when is_float(value), do: :gleam@json.float(value)
  defp to_gleam_json(value) when is_binary(value), do: :gleam@json.string(value)

  defp to_gleam_json(value) when is_list(value),
    do: :gleam@json.preprocessed_array(Enum.map(value, &to_gleam_json/1))

  defp to_gleam_json(%{} = value),
    do: :gleam@json.object(Enum.map(value, fn {k, v} -> {k, to_gleam_json(v)} end))

  defp parse_models(models) when is_list(models) and models != [] do
    Enum.map(models, fn model ->
      %{
        alias: fetch!(model, "alias"),
        name: model["name"] || fetch!(model, "alias"),
        description: model["description"] || "",
        model: fetch!(model, "model")
      }
    end)
  end

  defp parse_models(_), do: raise("Config needs at least one [[models]] entry")

  defp default_alias(nil, [first | _]), do: first.alias

  defp default_alias(alias_name, models) do
    unless Enum.any?(models, &(&1.alias == alias_name)) do
      raise "default_model \"#{alias_name}\" does not match any [[models]] alias"
    end

    alias_name
  end

  # models.Catalog / models.ModelAlias — field order is the FFI contract.
  # No pricing and no credit multipliers: a standalone deployment bills
  # nothing.
  defp catalog(models, default_alias) do
    aliases =
      Enum.map(models, fn model ->
        {:model_alias, model.alias, model.name, model.model, model.description, true, :none,
         :none}
      end)

    {:catalog, aliases, default_alias, fn _model_id -> :none end}
  end

  # One validated Options per model, keyed by the resolved provider model
  # ID (the catalog's model_id). Validation errors are operator-facing
  # boot failures.
  defp options_by_model(models, endpoint, api_key, headers, extra_body) do
    Map.new(models, fn model ->
      case :atuin_ai_core@llm@openai_endpoint.options(
             endpoint,
             api_key,
             headers,
             extra_body,
             model.model
           ) do
        {:ok, options} -> {model.model, options}
        {:error, message} -> raise "Invalid config for model \"#{model.alias}\": #{message}"
      end
    end)
  end
end
