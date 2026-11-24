//! Read-only access to Atuin's history database.
//!
//! The agent never writes Atuin's history: it opens `history.db` read-only and runs no
//! migrations, so an Atuin newer than this client can keep moving the schema forward. Only the
//! columns below are read; a database older than Atuin 18.13 (no `author`) can't be searched.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteRow};
use sqlx::{QueryBuilder, Row as _, Sqlite};
use time::OffsetDateTime;

/// Authors that are AI agents, by the name their Atuin integration records.
const KNOWN_AGENTS: &[&str] =
    &["amp", "claude-code", "codex", "copilot", "grok", "hermes", "kiro", "omp", "opencode", "pi"];

/// The user half Atuin gives a legacy `hostname` that has none.
const UNKNOWN_USER: &str = "unknown-user";

const HISTORY_COLUMNS: &str =
    "id, timestamp, duration, exit, command, cwd, session, hostname, author, intent, author_kind";

/// Who ran a command, when the entry says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorKind {
    User,
    Agent,
}

impl AuthorKind {
    /// The kind stored as `author_kind`, or `None` for one a newer Atuin added.
    fn from_column(value: i64) -> Option<Self> {
        match value {
            1 => Some(Self::User),
            2 => Some(Self::Agent),
            _ => None,
        }
    }
}

/// One command from Atuin's history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct History {
    /// The history ID, as Atuin stores it (simple-form UUID).
    pub id: String,
    pub timestamp: OffsetDateTime,
    /// How long the command ran, in nanoseconds; negative while it is still running.
    pub duration: i64,
    /// Exit code; `-1` while the command is still running.
    pub exit: i64,
    pub command: String,
    pub cwd: String,
    pub session: String,
    /// `<host>:<user>`, though legacy rows may lack the user.
    pub hostname: String,
    /// Who ran the command: an agent's name, or the user's when nothing else was recorded.
    pub author: String,
    /// The intent an agent stated for running the command.
    pub intent: Option<String>,
    pub author_kind: Option<AuthorKind>,
}

impl History {
    /// Whether an AI agent ran this command.
    ///
    /// A recorded kind decides. Otherwise a known agent name means an agent, unless it is also
    /// the username the author defaulted to, so a user called `pi` isn't mistaken for the agent.
    #[must_use]
    pub fn is_agent(&self) -> bool {
        if let Some(kind) = self.author_kind {
            return kind == AuthorKind::Agent;
        }
        let defaulted = match self.hostname.split_once(':') {
            Some((host, UNKNOWN_USER)) => host,
            Some((_, user)) => user,
            None => &self.hostname,
        };
        KNOWN_AGENTS.contains(&self.author.as_str()) && self.author != defaulted
    }

    /// A finished command run by the user `me` on `box`, at the Unix epoch.
    #[cfg(test)]
    pub(crate) fn for_tests(command: &str) -> Self {
        Self {
            id: "018f011c9a0a70008000000000000001".to_owned(),
            timestamp: OffsetDateTime::UNIX_EPOCH,
            duration: 0,
            exit: 0,
            command: command.to_owned(),
            cwd: "/".to_owned(),
            session: String::new(),
            hostname: "box:me".to_owned(),
            author: "me".to_owned(),
            intent: None,
            author_kind: None,
        }
    }

    fn from_row(row: &SqliteRow) -> sqlx::Result<Self> {
        let hostname: String = row.try_get("hostname")?;
        let author = row
            .try_get::<Option<String>, _>("author")?
            .filter(|author| !author.trim().is_empty())
            .unwrap_or_else(|| match hostname.split_once(':') {
                Some((_, user)) => user.to_owned(),
                None => hostname.clone(),
            });
        let nanos: i64 = row.try_get("timestamp")?;
        let timestamp = OffsetDateTime::from_unix_timestamp_nanos(i128::from(nanos))
            .map_err(|err| sqlx::Error::Decode(Box::new(err)))?;

        Ok(Self {
            id: row.try_get("id")?,
            timestamp,
            duration: row.try_get("duration")?,
            exit: row.try_get("exit")?,
            command: row.try_get("command")?,
            cwd: row.try_get("cwd")?,
            session: row.try_get("session")?,
            hostname,
            author,
            intent: row
                .try_get::<Option<String>, _>("intent")?
                .filter(|intent| !intent.trim().is_empty()),
            author_kind: row
                .try_get::<Option<i64>, _>("author_kind")?
                .and_then(AuthorKind::from_column),
        })
    }
}

/// How a query's terms match a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchMode {
    /// Every term appears as written.
    FullText,
    /// Every term's characters appear in order, not necessarily together.
    Fuzzy,
}

/// Which commands a search considers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterMode {
    Global,
    /// Run on this machine.
    Host,
    /// Run in this shell session.
    Session,
    /// Run in the current directory.
    Directory,
    /// Run in the current repository, or the current directory outside one.
    Workspace,
}

/// Where a search is run from, which the narrower [`FilterMode`]s scope to.
#[derive(Clone, Debug)]
pub struct Context {
    /// `$ATUIN_SESSION`, empty outside a shell Atuin hooks.
    pub session: String,
    pub cwd: String,
    /// `$ATUIN_HOST_NAME`, else the machine's hostname.
    pub host: String,
    pub vcs_root: Option<PathBuf>,
}

impl Context {
    /// The context of this process, read the way Atuin reads it when it records a command.
    #[must_use]
    pub fn current() -> Self {
        // PWD keeps the symlinked path the user sees, where `current_dir` resolves it.
        let cwd = std::env::var_os("PWD")
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let host = std::env::var("ATUIN_HOST_NAME")
            .ok()
            .or_else(|| whoami::hostname().ok())
            .unwrap_or_else(|| "unknown-host".to_owned());

        Self {
            session: std::env::var("ATUIN_SESSION").unwrap_or_default(),
            vcs_root: crate::vcs::root(&cwd),
            cwd: cwd.to_string_lossy().into_owned(),
            host,
        }
    }

    /// The [`FilterMode::Workspace`] root: the repository when there is one, else the cwd.
    fn workspace_root(&self) -> String {
        match &self.vcs_root {
            Some(root) => root.to_string_lossy().into_owned(),
            None => self.cwd.clone(),
        }
    }
}

/// Atuin's history database, opened read-only.
#[derive(Debug)]
pub struct HistoryDb {
    pool: SqlitePool,
}

impl HistoryDb {
    /// Open the database at `path`, waiting up to `timeout` on a lock.
    pub async fn open(path: &Path, timeout: Duration) -> sqlx::Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .read_only(true)
            .create_if_missing(false)
            .busy_timeout(timeout);
        let pool = SqlitePoolOptions::new().acquire_timeout(timeout).connect_with(options).await?;
        Ok(Self { pool })
    }

    /// The most recently finished command, if any.
    pub async fn last(&self) -> sqlx::Result<Option<History>> {
        let mut query = QueryBuilder::<Sqlite>::new(format!(
            "SELECT {HISTORY_COLUMNS} FROM history WHERE duration >= 0 AND deleted_at IS NULL \
             ORDER BY timestamp DESC LIMIT 1"
        ));
        let row = query.build().fetch_optional(&self.pool).await?;
        row.as_ref().map(History::from_row).transpose()
    }

    /// The newest run of each distinct command matching `query`, newest first.
    ///
    /// Fuzzy results come back ranked by [`rank_fuzzy`] instead.
    pub async fn search(
        &self,
        mode: SearchMode,
        filter: FilterMode,
        context: &Context,
        query: &str,
        limit: i64,
    ) -> sqlx::Result<Vec<History>> {
        // Each command's newest entry, as a correlated NOT EXISTS rather than a GROUP BY, so the
        // timestamp-ordered scan stops after `limit` commands instead of aggregating the table.
        let mut sql =
            QueryBuilder::<Sqlite>::new(format!("SELECT {HISTORY_COLUMNS} FROM history f"));
        push_conditions(&mut sql, "f", mode, filter, context, query);
        sql.push(" AND NOT EXISTS (SELECT 1 FROM history f2");
        push_conditions(&mut sql, "f2", mode, filter, context, query);
        sql.push(
            " AND f2.command = f.command AND (f2.timestamp, f2.id) > (f.timestamp, f.id)) ORDER \
             BY f.timestamp DESC LIMIT ",
        );
        sql.push_bind(limit);

        let rows = sql.build().fetch_all(&self.pool).await?;
        let results = rows.iter().map(History::from_row).collect::<sqlx::Result<Vec<_>>>()?;
        Ok(match mode {
            SearchMode::FullText => results,
            SearchMode::Fuzzy => rank_fuzzy(query, results),
        })
    }
}

/// `WHERE` conditions over `table` for a search, binding every value.
fn push_conditions(
    sql: &mut QueryBuilder<Sqlite>,
    table: &str,
    mode: SearchMode,
    filter: FilterMode,
    context: &Context,
    query: &str,
) {
    sql.push(format!(" WHERE {table}.deleted_at IS NULL"));

    match filter {
        FilterMode::Global => {}
        FilterMode::Host => {
            // `hostname` is `<host>:<user>`, or a bare `<host>` in rows some importers wrote.
            let host = context.host.to_lowercase();
            sql.push(format!(" AND (lower({table}.hostname) = "));
            sql.push_bind(host.clone());
            sql.push(format!(" OR (lower({table}.hostname) >= "));
            sql.push_bind(format!("{host}:"));
            sql.push(format!(" AND lower({table}.hostname) < "));
            sql.push_bind(format!("{host};"));
            sql.push("))");
        }
        FilterMode::Session => {
            sql.push(format!(" AND {table}.session = "));
            sql.push_bind(context.session.clone());
        }
        FilterMode::Directory => {
            sql.push(format!(" AND {table}.cwd = "));
            sql.push_bind(context.cwd.clone());
        }
        FilterMode::Workspace => {
            let root = context.workspace_root();
            // A bare `LIKE 'root%'` would also match a sibling sharing the prefix.
            let prefix = if root.ends_with(std::path::MAIN_SEPARATOR) {
                root.clone()
            } else {
                format!("{root}{}", std::path::MAIN_SEPARATOR)
            };
            sql.push(format!(" AND ({table}.cwd = "));
            sql.push_bind(root);
            sql.push(format!(" OR {table}.cwd LIKE "));
            sql.push_bind(format!("{}%", escape_like(&prefix)));
            sql.push(" ESCAPE '\\')");
        }
    }

    for term in query.split_whitespace() {
        // Smart case, as Atuin's search: a term with an uppercase letter matches case-sensitively.
        if term.chars().any(char::is_uppercase) {
            let pattern = match mode {
                SearchMode::FullText => format!("*{}*", escape_glob(term)),
                SearchMode::Fuzzy => spread(term, "*", escape_glob),
            };
            sql.push(format!(" AND {table}.command GLOB "));
            sql.push_bind(pattern);
        } else {
            let pattern = match mode {
                SearchMode::FullText => format!("%{}%", escape_like(term)),
                SearchMode::Fuzzy => spread(term, "%", escape_like),
            };
            sql.push(format!(" AND {table}.command LIKE "));
            sql.push_bind(pattern);
            sql.push(" ESCAPE '\\'");
        }
    }
}

/// `term`'s characters, each escaped, with `wildcard` around and between them: `%a%b%`.
fn spread(term: &str, wildcard: &str, escape: fn(&str) -> String) -> String {
    let mut pattern = wildcard.to_owned();
    let mut buf = [0; 4];
    for ch in term.chars() {
        pattern.push_str(&escape(ch.encode_utf8(&mut buf)));
        pattern.push_str(wildcard);
    }
    pattern
}

/// Escape `LIKE`'s wildcards for `ESCAPE '\'`.
fn escape_like(s: &str) -> String {
    s.replace('\\', r"\\").replace('%', r"\%").replace('_', r"\_")
}

/// Escape `GLOB`'s wildcards by wrapping each in a character class.
fn escape_glob(s: &str) -> String {
    s.chars().fold(String::with_capacity(s.len()), |mut out, ch| {
        match ch {
            '*' | '?' | '[' => {
                out.push('[');
                out.push(ch);
                out.push(']');
            }
            _ => out.push(ch),
        }
        out
    })
}

/// Whether fuzzy and full-text search can differ for `query`: only when it has a term.
#[must_use]
pub fn has_fuzzy_terms(query: &str) -> bool {
    query.split_whitespace().next().is_some()
}

/// Rank `results` tightest match first: by the shortest span of each command that holds the
/// query's characters in order. A command that doesn't hold them sorts last.
#[must_use]
pub fn rank_fuzzy(query: &str, mut results: Vec<History>) -> Vec<History> {
    let query: Vec<char> = query.split_whitespace().flat_map(str::chars).collect();
    results.sort_by_cached_key(|history| {
        let command: Vec<char> = history.command.chars().collect();
        minspan::minspan::span(&query, &command).map_or(usize::MAX, |(from, to)| 1 + to - from)
    });
    results
}

#[cfg(test)]
mod tests {
    use rstest::{fixture, rstest};
    use sqlx::Executor as _;

    use super::*;

    fn history(command: &str) -> History {
        History::for_tests(command)
    }

    fn context() -> Context {
        Context {
            session: "s1".to_owned(),
            cwd: "/repo/src".to_owned(),
            host: "Box".to_owned(),
            vcs_root: Some(PathBuf::from("/repo")),
        }
    }

    /// Rows as `(command, cwd, session, hostname, timestamp)`.
    const ROWS: &[(&str, &str, &str, &str, i64)] = &[
        ("cargo test", "/repo", "s1", "box:me", 1),
        ("cargo build --release", "/repo/src", "s2", "box:me", 2),
        ("cargo test", "/repo/src", "s2", "other:me", 3),
        ("git push", "/repository", "s1", "box", 4),
        ("Cargo.toml edit", "/", "s3", "other:me", 5),
        ("echo 100%_done", "/", "s3", "box:me", 6),
    ];

    /// A database with Atuin's history columns and [`ROWS`], plus a deleted row.
    #[fixture]
    async fn db() -> (tempfile::TempDir, HistoryDb) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let options = SqliteConnectOptions::new().filename(&path).create_if_missing(true);
        let pool = SqlitePool::connect_with(options).await.unwrap();
        pool.execute(
            "CREATE TABLE history (id text primary key, timestamp integer not null, duration \
             integer not null, exit integer not null, command text not null, cwd text not null, \
             session text not null, hostname text not null, deleted_at integer, author text, \
             intent text, shell text, author_kind integer)",
        )
        .await
        .unwrap();
        for (i, (command, cwd, session, hostname, timestamp)) in ROWS.iter().enumerate() {
            sqlx::query(
                "INSERT INTO history (id, timestamp, duration, exit, command, cwd, session, \
                 hostname) VALUES (?, ?, 1, 0, ?, ?, ?, ?)",
            )
            .bind(format!("id{i}"))
            .bind(timestamp)
            .bind(command)
            .bind(cwd)
            .bind(session)
            .bind(hostname)
            .execute(&pool)
            .await
            .unwrap();
        }
        pool.execute(
            "INSERT INTO history (id, timestamp, duration, exit, command, cwd, session, hostname, \
             deleted_at) VALUES ('gone', 9, 1, 0, 'cargo deleted', '/', 's1', 'box:me', 9)",
        )
        .await
        .unwrap();
        pool.close().await;

        let db = HistoryDb::open(&path, Duration::from_secs(1)).await.unwrap();
        (dir, db)
    }

    fn commands(results: &[History]) -> Vec<&str> {
        results.iter().map(|h| h.command.as_str()).collect()
    }

    #[rstest]
    #[case::global_and_case_insensitive(
        SearchMode::FullText, FilterMode::Global, "cargo", vec!["Cargo.toml edit", "cargo test", "cargo build --release"]
    )]
    #[case::smart_case(SearchMode::FullText, FilterMode::Global, "Cargo", vec!["Cargo.toml edit"])]
    #[case::terms_are_anded(SearchMode::FullText, FilterMode::Global, "cargo release", vec!["cargo build --release"])]
    #[case::fuzzy(SearchMode::Fuzzy, FilterMode::Global, "gtps", vec!["git push"])]
    #[case::like_wildcards_are_literal(SearchMode::FullText, FilterMode::Global, "0%_", vec!["echo 100%_done"])]
    #[case::host_is_case_insensitive_and_matches_bare_hosts(
        SearchMode::FullText, FilterMode::Host, "", vec!["echo 100%_done", "git push", "cargo build --release", "cargo test"]
    )]
    #[case::session(SearchMode::FullText, FilterMode::Session, "", vec!["git push", "cargo test"])]
    #[case::directory(SearchMode::FullText, FilterMode::Directory, "cargo", vec!["cargo test", "cargo build --release"])]
    #[case::workspace_skips_siblings(SearchMode::FullText, FilterMode::Workspace, "", vec!["cargo test", "cargo build --release"])]
    #[tokio::test]
    async fn search(
        #[future] db: (tempfile::TempDir, HistoryDb),
        #[case] mode: SearchMode,
        #[case] filter: FilterMode,
        #[case] query: &str,
        #[case] expected: Vec<&str>,
    ) {
        let (_dir, db) = db.await;
        let results = db.search(mode, filter, &context(), query, 10).await.unwrap();
        assert_eq!(commands(&results), expected);
    }

    #[rstest]
    #[tokio::test]
    async fn search_keeps_each_command_s_newest_run(#[future] db: (tempfile::TempDir, HistoryDb)) {
        let (_dir, db) = db.await;
        let results = db
            .search(SearchMode::FullText, FilterMode::Global, &context(), "cargo test", 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].cwd, "/repo/src");
    }

    #[rstest]
    #[tokio::test]
    async fn last_skips_deleted_commands(#[future] db: (tempfile::TempDir, HistoryDb)) {
        let (_dir, db) = db.await;
        assert_eq!(db.last().await.unwrap().unwrap().command, "echo 100%_done");
    }

    #[rstest]
    fn fuzzy_ranks_the_tightest_match_first() {
        let newest_first = ["ffmpeg -i in.mkv out.mkv # convert later to av1", "ffmpeg -c:v av1 x"];
        let ranked = rank_fuzzy("ffmpeg av1", newest_first.into_iter().map(history).collect());
        assert_eq!(commands(&ranked), ["ffmpeg -c:v av1 x", newest_first[0]]);
    }

    #[rstest]
    fn fuzzy_sorts_a_non_match_last() {
        let ranked = rank_fuzzy("screen", ["hello", "screen"].into_iter().map(history).collect());
        assert_eq!(commands(&ranked), ["screen", "hello"]);
    }

    #[rstest]
    #[case::recorded_agent(Some(AuthorKind::Agent), "anyone", "box:me", true)]
    #[case::recorded_user(Some(AuthorKind::User), "claude-code", "box:me", false)]
    #[case::known_agent_name(None, "codex", "box:me", true)]
    #[case::user_named_like_an_agent(None, "pi", "box:pi", false)]
    #[case::legacy_host_named_like_an_agent(None, "pi", "pi:unknown-user", false)]
    #[case::unknown_name(None, "deploy-bot", "box:me", false)]
    fn is_agent(
        #[case] kind: Option<AuthorKind>,
        #[case] author: &str,
        #[case] hostname: &str,
        #[case] expected: bool,
    ) {
        let mut h = history("x");
        h.author_kind = kind;
        h.author = author.to_owned();
        h.hostname = hostname.to_owned();
        assert_eq!(h.is_agent(), expected);
    }
}
