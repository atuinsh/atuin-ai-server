use time::UtcOffset;
use time::macros::format_description;

use crate::history::History;

/// The local UTC offset, or UTC when it can't be determined.
#[must_use]
pub fn local_offset() -> UtcOffset {
    UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC)
}

pub fn format_last_command(history: &History, local_offset: UtcOffset) -> String {
    format!(
        "History ID: {} - `{}`\n{}",
        history.id,
        history.command,
        format_history_metadata(history, local_offset)
    )
}

/// How much of a command a search result shows. Agent-run commands are often multi-kilobyte
/// heredocs or scripts; shown whole, a page of 30 results can run past 100k characters, which
/// floods a model's context for what is a lookup. The opening shows what the command was.
const SEARCH_RESULT_COMMAND_CHARS: usize = 600;

pub fn format_history_search_result(
    ordinal: usize,
    history: &History,
    local_offset: UtcOffset,
) -> String {
    format!(
        "## #{}. (History ID: {}):\n`{}`\n{}\n",
        ordinal,
        history.id,
        clip_command(&history.command),
        format_history_metadata(history, local_offset)
    )
}

fn clip_command(command: &str) -> std::borrow::Cow<'_, str> {
    let head = match command.char_indices().nth(SEARCH_RESULT_COMMAND_CHARS) {
        Some((end, _)) => &command[..end],
        None => command,
    };
    if head.len() == command.len() {
        return command.into();
    }
    format!("{head}… [+{} chars]", command[head.len()..].chars().count()).into()
}

fn format_history_metadata(history: &History, local_offset: UtcOffset) -> String {
    format!(
        "[{}] (in `{}`, exit {}){}{}",
        format_timestamp(history, local_offset),
        history.cwd,
        history.exit,
        format_duration(history.duration),
        format_attribution(history)
    )
}

/// Attribution for agent-run commands: which agent, and its stated intent.
/// A stated intent marks a command as agent-run even when the entry is not
/// recognised as one, so its author is still named. User-run commands (no
/// intent, not an agent) get nothing.
fn format_attribution(history: &History) -> String {
    match (history.is_agent(), &history.intent) {
        (true, Some(intent)) => format!(" — {}: {intent}", history.author),
        (true, None) => format!(" — {}", history.author),
        (false, Some(intent)) if !history.author.is_empty() => {
            format!(" — {}: {intent}", history.author)
        }
        (false, Some(intent)) => format!(" — intent: {intent}"),
        (false, None) => String::new(),
    }
}

/// `2024-01-22 14:35:07`.
fn format_timestamp(history: &History, local_offset: UtcOffset) -> String {
    let format = format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
    history.timestamp.to_offset(local_offset).format(format).unwrap_or_default()
}

fn format_duration(nanos: i64) -> String {
    let Ok(nanos @ 1..) = u64::try_from(nanos) else {
        return String::new();
    };
    format!(", {}", stopwatch(std::time::Duration::from_nanos(nanos)))
}

/// A duration as a stopwatch shows it, keeping milliseconds under a minute and rolling up to
/// hours at most: `814ms`, `1.234s`, `1m30s`, `72h0m0s`.
fn stopwatch(duration: std::time::Duration) -> String {
    let secs = duration.as_secs();
    let millis = duration.subsec_millis();
    match secs {
        3600.. => format!("{}h{}m{}s", secs / 3600, secs % 3600 / 60, secs % 60),
        60.. => format!("{}m{}s", secs / 60, secs % 60),
        1.. if millis > 0 => format!("{secs}.{millis:03}s"),
        1.. => format!("{secs}s"),
        0 => format!("{millis}ms"),
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use time::UtcOffset;

    use super::*;

    fn history(duration: i64) -> History {
        History {
            duration,
            exit: 2,
            cwd: "/repo".to_string(),
            hostname: "box".to_string(),
            author: String::new(),
            ..History::for_tests("cargo test")
        }
    }

    #[rstest]
    fn search_results_clip_long_commands() {
        let mut long = history(0);
        long.command = format!("cat <<'EOF'\n{}\nEOF", "x".repeat(5_000));
        let out = format_history_search_result(1, &long, UtcOffset::UTC);
        assert!(out.len() < 1_000, "{}", out.len());
        assert!(out.contains("… [+4"), "{out}");
        assert!(
            format_history_search_result(1, &history(0), UtcOffset::UTC).contains("`cargo test`")
        );
    }

    #[rstest]
    fn formats_last_command() {
        assert_eq!(
            format_last_command(&history(1_234_000_000), UtcOffset::UTC),
            "History ID: 018f011c9a0a70008000000000000001 - `cargo test`\n[1970-01-01 00:00:00] \
             (in `/repo`, exit 2), 1.234s"
        );
    }

    #[rstest]
    fn formats_history_search_result() {
        assert_eq!(
            format_history_search_result(3, &history(0), UtcOffset::UTC),
            "## #3. (History ID: 018f011c9a0a70008000000000000001):\n`cargo test`\n[1970-01-01 \
             00:00:00] (in `/repo`, exit 2)\n"
        );
    }

    #[rstest]
    #[case::zero(0, "0ms")]
    #[case::sub_second_only(814_000_000, "814ms")]
    #[case::whole_second(1_000_000_000, "1s")]
    #[case::fractional_second(1_234_000_000, "1.234s")]
    #[case::minutes(90_000_000_000, "1m30s")]
    #[case::hours(3_723_000_000_000, "1h2m3s")]
    #[case::days_stay_in_hours(259_200_000_000_000, "72h0m0s")]
    fn stopwatch_keeps_subsecond_resolution(#[case] nanos: u64, #[case] expected: &str) {
        assert_eq!(stopwatch(std::time::Duration::from_nanos(nanos)), expected);
    }

    #[rstest]
    #[case::known_with_intent(
        "claude-code",
        Some("Run the test suite"),
        " — claude-code: Run the test suite"
    )]
    #[case::known_no_intent("codex", None, " — codex")]
    #[case::unknown_author_with_intent("deploy-bot", Some("ship it"), " — deploy-bot: ship it")]
    #[case::empty_author_with_intent("", Some("mystery"), " — intent: mystery")]
    #[case::user_no_intent("ellie", None, "")]
    fn attribution_matrix(
        #[case] author: &str,
        #[case] intent: Option<&str>,
        #[case] expected: &str,
    ) {
        let mut h = history(0);
        h.author = author.to_string();
        h.intent = intent.map(String::from);
        assert_eq!(format_attribution(&h), expected);
    }
}
