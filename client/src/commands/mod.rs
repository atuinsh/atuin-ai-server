use clap::{Parser, Subcommand, ValueEnum};
use secrecy::SecretString;

use crate::settings::Settings;

mod inline;

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
    Inline {
        /// Enable verbose logging
        #[arg(short, long)]
        verbose: bool,

        /// Custom API endpoint; defaults to reading from the `ai.endpoint` setting.
        #[arg(long)]
        api_endpoint: Option<String>,

        /// Custom API token; defaults to reading from the `ai.api_token` setting.
        #[arg(long)]
        api_token: Option<SecretString>,

        /// The first message to send
        #[arg(value_name = "PROMPT")]
        prompt: Option<String>,

        /// Answer the shell widget on stderr
        #[arg(long, hide = true)]
        hook: bool,
    },

    /// Print the shell integration that binds `?` at an empty prompt
    ///
    /// Evaluate it after `atuin init`, e.g. `eval "$(atuin-ai init zsh)"` in `~/.zshrc`.
    Init {
        shell: InitShell,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum InitShell {
    Zsh,
    Bash,
    Fish,
}

pub async fn run(command: Command, settings: Settings) -> eyre::Result<()> {
    match command {
        Command::Inline {
            api_endpoint,
            api_token,
            prompt,
            hook,
            ..
        } => inline::run(prompt, api_endpoint, api_token, settings, hook).await,
        Command::Init { shell } => {
            if settings.ai.enabled == Some(false) {
                // Valid comment syntax, and so a no-op, in every shell this supports.
                println!("# Atuin AI is disabled: `ai.enabled = false` in config.toml.");
                return Ok(());
            }
            println!("{}", crate::shell::init(shell));
            Ok(())
        }
    }
}
