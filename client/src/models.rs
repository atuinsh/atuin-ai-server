//! Model listing and selection.
//!
//! The server lists the models it serves at `/api/cli/models`.
//! Aliases are what we send on the wire; names and descriptions are for
//! display in the `/model` picker.

use std::path::Path;
use std::time::Duration;

use eyre::{Context, Result};
use reqwest::header::USER_AGENT;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ModelInfo {
    pub alias: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ModelList {
    /// Alias the server uses when a request doesn't specify a model.
    pub default: String,
    pub models: Vec<ModelInfo>,
}

/// Fetch the models available to this user. Sent authenticated because the
/// server includes feature-flag-gated models only for entitled users.
pub async fn fetch_models(
    endpoint: &reqwest::Url,
    token: Option<&SecretString>,
) -> Result<ModelList> {
    let url = crate::stream::api_url(endpoint, "api/cli/models")?;

    let mut request = reqwest::Client::new()
        .get(url)
        .header(USER_AGENT, crate::stream::APP_USER_AGENT)
        .timeout(Duration::from_secs(10));
    if let Some(token) = token {
        request = request.bearer_auth(token.expose_secret());
    }
    let response = request.send().await.context("failed to fetch model list")?;

    let status = response.status();
    if !status.is_success() {
        eyre::bail!("model list request failed ({status})");
    }

    response.json::<ModelList>().await.context("failed to parse model list")
}

/// Persist the chosen alias to `model` in ai.toml so it becomes the default
/// for future sessions. Already-running sessions keep the model they read at
/// startup.
pub async fn save_model_selection(alias: &str) -> Result<()> {
    save_model_to(&crate::settings::config_file(), alias).await
}

/// Set `model` in the TOML file at `path`, keeping the rest of it, comments
/// included. A missing file is created.
async fn save_model_to(path: &Path, alias: &str) -> Result<()> {
    let existing = match tokio::fs::read_to_string(path).await {
        Ok(existing) => existing,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("failed to read {}", path.display())),
    };
    let mut doc = existing
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("failed to parse {}", path.display()))?;
    doc["model"] = toml_edit::value(alias);

    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    tokio::fs::write(path, doc.to_string())
        .await
        .with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::new_file(None)]
    #[case::replaces_the_old_choice(Some("model = \"old\"\n"))]
    #[case::keeps_the_rest(Some(
        "# my server\nendpoint = \"http://localhost:8080\"\n\n[opening]\nsend_cwd = true\n"
    ))]
    #[tokio::test]
    async fn saves_the_model_at_the_top_level(#[case] existing: Option<&str>) {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("ai.toml");
        if let Some(existing) = existing {
            std::fs::write(&path, existing).expect("the file is written");
        }

        save_model_to(&path, "llama").await.expect("the model is saved");

        let saved = std::fs::read_to_string(&path).expect("the file is read");
        let doc: toml::Table = saved.parse().expect("the result parses");
        assert_eq!(doc["model"].as_str(), Some("llama"), "{saved}");
        let unchanged = existing.unwrap_or_default().lines().filter(|line| !line.contains("model"));
        for line in unchanged {
            assert!(saved.contains(line), "lost {line:?}:\n{saved}");
        }
    }
}
