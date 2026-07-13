defmodule Mix.Tasks.Compile.Gleam do
  @shortdoc "Compiles the shared CLI chat engine"
  @moduledoc """
  Mix compiler for the shared Gleam engine, fetched by mix as an inert
  git dependency (`compile: false, app: false` — mix owns fetching and
  pinning, gleam owns building, this task bridges). Registered via
  `compilers: [:gleam] ++ Mix.compilers()` so the engine is built and on
  the code path before Elixir compiles. Defined here in
  mix.exs (not lib/) because project code isn't compiled yet when the
  compiler list is first consulted. `normalize_app_specs!/0` rewrites the
  gleam-generated `.app` files (populating `modules`, stripping dev-deps)
  so Mix releases pick them up as first-class OTP apps and embedded mode
  preloads their modules via the boot script.
  """

  use Mix.Task.Compiler

  @core_dir "deps/atuin_ai_core"
  # `gleam build` only has a dev profile; the release reads each app's
  # `.app` straight from this directory after normalize_app_specs!/0.
  @build_dir Path.join([@core_dir, "build", "dev", "erlang"])

  # The core is built as the root gleam project here, so its
  # dev-dependencies get built too; exclude them from the code path and
  # the release.
  @dev_only_gleam_apps [:gleeunit, :simplifile, :envoy]

  @impl Mix.Task.Compiler
  def run(_args) do
    unless System.find_executable("gleam") do
      Mix.raise("`gleam` was not found on PATH.")
    end

    unless File.dir?(@core_dir) do
      Mix.raise("The engine dependency is missing; run `mix deps.get` first.")
    end

    case System.cmd("gleam", ["build"], cd: @core_dir, stderr_to_stdout: true) do
      {output, 0} ->
        unless String.contains?(output, "Compiled in 0.") do
          Mix.shell().info(String.trim(output))
        end

        normalize_app_specs!()
        add_code_paths()
        {:ok, []}

      {output, _status} ->
        Mix.raise("Gleam build failed:\n#{output}")
    end
  end

  def add_code_paths do
    Enum.each(ebin_dirs(), &Code.prepend_path/1)
  end

  # gleam leaves `modules` empty in each `.app` (it expects interactive
  # mode to autoload) and lists dev-deps under `applications`. Embedded
  # mode only preloads modules named in the boot script, which systools
  # builds from the `.app`'s `modules` field, so an empty list means
  # nothing loads. Rewrite each non-dev app's spec in place: populate
  # `modules` (excluding test modules), drop dev-only deps from
  # `applications`. Idempotent: `gleam build` rewrites each `.app` fresh
  # on every run, so we always normalize from gleam's original output.
  def normalize_app_specs! do
    for ebin <- ebin_dirs() do
      rewrite_app_file!(ebin, Path.basename(Path.dirname(ebin)))
    end
  end

  defp rewrite_app_file!(ebin, app) when is_binary(app) do
    app_atom = String.to_atom(app)
    app_file = Path.join(ebin, "#{app}.app")
    {:ok, [{:application, ^app_atom, props}]} = :file.consult(app_file)

    modules =
      ebin
      |> File.ls!()
      |> Enum.filter(&String.ends_with?(&1, ".beam"))
      |> Enum.map(&String.trim_trailing(&1, ".beam"))
      |> Enum.map(&String.to_atom/1)
      |> Enum.reject(&test_module?/1)
      |> Enum.sort()

    runtime_apps =
      props
      |> Keyword.get(:applications, [])
      |> Enum.reject(&(&1 in @dev_only_gleam_apps))

    props =
      props
      |> Keyword.put(:modules, modules)
      |> Keyword.put(:applications, runtime_apps)

    contents = :io_lib.format("~p.~n", [{:application, app_atom, props}])
    File.write!(app_file, contents)
  end

  # gleam emits test modules (`foo_test`) into the same ebin as production
  # code; keep them out of the boot script's preload list.
  defp test_module?(mod) do
    String.ends_with?(Atom.to_string(mod), "_test")
  end

  defp ebin_dirs do
    case File.ls(Path.expand(@build_dir)) do
      {:ok, apps} ->
        for app <- apps,
            String.to_atom(app) not in @dev_only_gleam_apps,
            ebin = Path.join([Path.expand(@build_dir), app, "ebin"]),
            File.dir?(ebin),
            do: ebin

      {:error, _} ->
        []
    end
  end
end

defmodule AtuinAI.Server.MixProject do
  use Mix.Project

  def project do
    [
      app: :atuin_ai_server,
      version: "0.1.0",
      elixir: "~> 1.18",
      compilers: [:gleam] ++ Mix.compilers(),
      start_permanent: Mix.env() == :prod,
      deps: deps(),
      releases: [
        # Gleam apps are normalized at compile time
        # (Mix.Tasks.Compile.Gleam.normalize_app_specs!/0) so the `.app`
        # files Mix reads during assembly have `modules` populated.
        # `atuin_ai_core: :load` puts the engine in the boot script's
        # application list (code-only, no supervision tree); its gleam
        # dependencies ride in transitively via its `.app` requirements.
        atuin_ai_server: [
          applications: [atuin_ai_core: :load],
          include_executables_for: [:unix]
        ]
      ]
    ]
  end

  def application do
    [
      mod: {AtuinAI.Server.Application, []},
      # :inets for the httpc-based e2e tests.
      extra_applications: [:logger, :inets]
    ]
  end

  defp deps do
    [
      {:atuin_ai_core,
       git: "https://github.com/atuinsh/atuin-ai-core.git",
       tag: "v0.1.0",
       compile: false,
       app: false},
      {:bandit, "~> 1.5"},
      {:plug, "~> 1.16"},
      {:toml, "~> 0.7"}
    ]
  end
end
