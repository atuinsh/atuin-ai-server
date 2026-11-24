use clap::{Args, Parser, Subcommand, ValueEnum};
use secrecy::SecretString;

pub mod inline;

/// The Atuin AI shell agent.
#[derive(Parser, Debug)]
#[command(version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Chat with the agent in a small TUI below the prompt
    Inline(InlineArgs),

    /// Print the shell integration that binds `?` at an empty prompt
    ///
    /// Evaluate it after `atuin init`, e.g. `eval "$(atuin-ai init zsh)"` in `~/.zshrc`.
    Init {
        shell: InitShell,
    },
}

#[derive(Args, Debug)]
pub struct InlineArgs {
    /// Enable verbose logging
    #[arg(short, long)]
    pub verbose: bool,

    /// Custom API endpoint; defaults to `endpoint` in ai.toml.
    #[arg(long)]
    pub api_endpoint: Option<String>,

    /// Custom API token; defaults to `api_token` in ai.toml.
    #[arg(long)]
    pub api_token: Option<SecretString>,

    /// The first message to send
    #[arg(value_name = "PROMPT")]
    pub prompt: Option<String>,

    /// Answer the shell widget on stderr
    #[arg(long, hide = true)]
    pub hook: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum InitShell {
    Zsh,
    Bash,
    Fish,
}
