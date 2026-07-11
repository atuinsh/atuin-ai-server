defmodule Mix.Tasks.Compile.Gleam do
  @shortdoc "Compiles the shared CLI chat engine"
  @moduledoc """
  Mix compiler for the shared Gleam engine in `../gleam_cli_chat_core`.
  Registered via `compilers: [:gleam] ++ Mix.compilers()` so the engine is
  built and on the code path before Elixir compiles. Defined here in
  mix.exs (not lib/) because project code isn't compiled yet when the
  compiler list is first consulted.
  """

  use Mix.Task.Compiler

  @core_dir "../gleam_cli_chat_core"
  @build_dir Path.join([@core_dir, "build", "dev", "erlang"])

  # Built by `gleam build` as the core's dev-dependencies but not needed
  # (or wanted) on this app's code path.
  @dev_only_gleam_apps [:gleeunit, :simplifile, :envoy]

  @impl Mix.Task.Compiler
  def run(_args) do
    unless System.find_executable("gleam") do
      Mix.raise("`gleam` was not found on PATH.")
    end

    case System.cmd("gleam", ["build"], cd: @core_dir, stderr_to_stdout: true) do
      {output, 0} ->
        unless String.contains?(output, "Compiled in 0.") do
          Mix.shell().info(String.trim(output))
        end

        add_code_paths()
        {:ok, []}

      {output, _status} ->
        Mix.raise("Gleam build failed:\n#{output}")
    end
  end

  def add_code_paths do
    case File.ls(Path.expand(@build_dir)) do
      {:ok, apps} ->
        for app <- apps,
            String.to_atom(app) not in @dev_only_gleam_apps,
            ebin = Path.join([Path.expand(@build_dir), app, "ebin"]),
            File.dir?(ebin) do
          Code.prepend_path(ebin)
        end

      {:error, _} ->
        :ok
    end
  end
end

defmodule CliChatStandalone.MixProject do
  use Mix.Project

  def project do
    [
      app: :cli_chat_standalone,
      version: "0.1.0",
      elixir: "~> 1.18",
      compilers: [:gleam] ++ Mix.compilers(),
      start_permanent: Mix.env() == :prod,
      deps: deps()
    ]
  end

  def application do
    [
      mod: {CliChatStandalone.Application, []},
      # :inets for the httpc-based e2e tests.
      extra_applications: [:logger, :inets]
    ]
  end

  defp deps do
    [
      {:bandit, "~> 1.5"},
      {:plug, "~> 1.16"},
      {:toml, "~> 0.7"}
    ]
  end
end
