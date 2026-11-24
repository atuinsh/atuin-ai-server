//! A width-aware spinner label that middle-elides a command instead of
//! clipping it at the right edge.
//!
//! eye-declare's `Spinner` hard-clips its label and `Text` word-wraps;
//! neither suits a long shell command on a status line, where the tail
//! (flags, paths) matters as much as the head. This element measures at
//! render time and splices an ellipsis into the middle so both ends stay
//! visible. Truncation is display-only — anywhere the user must review a
//! command in full (permission prompts for shell, suggested commands)
//! keeps the wrapping renderers.

use std::time::Duration;

use easy_cast::Conv;
use eye_declare::{Element, Spinner};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use unicode_width::UnicodeWidthStr as _;

use crate::ellipsis::{Budget, Elide, ellipsize};

/// Fit `command` into `cols` display columns on a single line: whitespace
/// runs (including newlines in multiline commands) collapse to single
/// spaces, and the middle is elided when it still doesn't fit.
fn fit_middle(command: &str, cols: usize) -> String {
    let one_line = command.split_whitespace().collect::<Vec<_>>().join(" ");
    ellipsize(&one_line, Budget::Columns(cols), Elide::Middle, "…").into_owned()
}

/// A tool spinner labeled `prefix` + command, the command middle-elided to
/// the render width. Styling matches [`super::tool_spinner`]; the done
/// checkmark is always hidden, as in the shell tool view.
pub struct CommandSpinner {
    prefix: &'static str,
    command: String,
    done: bool,
}

pub fn command_spinner(
    prefix: &'static str,
    command: impl Into<String>,
    done: bool,
) -> CommandSpinner {
    CommandSpinner {
        prefix,
        command: command.into(),
        done,
    }
}

impl CommandSpinner {
    fn spinner_at(&self, width: u16) -> Spinner {
        // Marker glyph + trailing space while animating; a done spinner
        // with a hidden checkmark has no marker (see Spinner::render).
        let marker_cols = if self.done {
            0
        } else {
            2
        };
        let budget =
            usize::conv(width).saturating_sub(marker_cols).saturating_sub(self.prefix.width());
        let label = format!("{}{}", self.prefix, fit_middle(&self.command, budget));
        super::tool_spinner(label, self.done).hide_checkmark()
    }
}

impl Element for CommandSpinner {
    fn height(&self, width: u16) -> u16 {
        if width == 0 {
            0
        } else {
            1
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.spinner_at(area.width).render(area, buf);
    }

    fn animated(&self) -> Option<Duration> {
        // Delegate so the frame interval stays in lockstep with Spinner's.
        self.spinner_at(0).animated()
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn render_line(el: &impl Element, width: u16) -> String {
        let area = Rect::new(0, 0, width, 1);
        let mut buf = Buffer::empty(area);
        el.render(area, &mut buf);
        (0..width)
            .map(|x| buf[(x, 0)].symbol().to_string())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    fn assert_middle_elided(rendered: &str, head: &str, tail: &str, max_width: usize) {
        assert!(rendered.starts_with(head), "head survives: {rendered}");
        assert!(rendered.ends_with(tail), "tail survives: {rendered}");
        assert!(rendered.contains('…'), "elision marked: {rendered}");
        assert!(rendered.width() <= max_width, "within width: {rendered}");
    }

    #[rstest]
    #[case::keeps_short_commands("cargo test", 20, "cargo test")]
    #[case::flattens_multiline(
        "for f in *.rs\ndo\n  wc -l \"$f\"\ndone",
        80,
        "for f in *.rs do wc -l \"$f\" done"
    )]
    fn fit_middle_exact(#[case] input: &str, #[case] cols: usize, #[case] expected: &str) {
        assert_eq!(fit_middle(input, cols), expected);
    }

    #[rstest]
    fn fit_middle_elides_the_middle() {
        let out = fit_middle("cargo test --workspace -- --nocapture history", 20);
        assert_middle_elided(&out, "cargo", "history", 20);
    }

    #[rstest]
    fn done_command_spinner_shows_both_ends() {
        let el = command_spinner("Ran: ", "git log --oneline --graph --all --decorate", true);
        let line = render_line(&el, 24);
        assert_middle_elided(&line, "Ran: git", "decorate", 24);
    }

    #[rstest]
    fn running_command_spinner_reserves_marker_columns() {
        let el = command_spinner("Running: ", "cargo build --release --locked", false);
        // The label budget accounts for the 2-column marker, so the command's
        // tail lands exactly at the right edge instead of getting clipped.
        let line = render_line(&el, 24);
        assert!(line.contains("Running: cargo"), "got: {line}");
        assert!(line.contains('…'), "got: {line}");
        assert!(line.ends_with("locked"), "got: {line}");
        assert_eq!(line.width(), 24, "got: {line}");
    }

    #[rstest]
    fn command_spinner_fits_untruncated_when_room() {
        let el = command_spinner("Ran: ", "ls -la", true);
        assert_eq!(render_line(&el, 40), "Ran: ls -la");
    }
}
