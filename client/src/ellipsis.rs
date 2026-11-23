//! Truncating a string to a budget, with a marker where content was dropped.

use std::borrow::Cow;

use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

/// How much room a string has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Budget {
    /// UTF-8 bytes.
    Bytes(usize),
    /// Display columns, where a double-width glyph such as `世` counts as two.
    Columns(usize),
}

impl Budget {
    const fn amount(self) -> usize {
        match self {
            Self::Bytes(n) | Self::Columns(n) => n,
        }
    }

    fn cost(self, s: &str) -> usize {
        match self {
            Self::Bytes(_) => s.len(),
            Self::Columns(_) => s.width(),
        }
    }
}

/// Which part of a string that doesn't fit is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elide {
    /// Keep both ends: `he…ld`.
    Middle,
    /// Keep the head: `hello…`.
    End,
}

/// `s` if it fits `budget`, else `s` with `marker` spliced in where content was dropped.
///
/// Cuts only between grapheme clusters. When even `marker` doesn't fit, the head is hard-truncated
/// with no marker.
#[must_use]
pub fn ellipsize<'a>(s: &'a str, budget: Budget, elide: Elide, marker: &str) -> Cow<'a, str> {
    let amount = budget.amount();
    if budget.cost(s) <= amount {
        return s.into();
    }

    let marker_cost = budget.cost(marker);
    let Some(content) = amount.checked_sub(marker_cost) else {
        return s[..fitting_prefix(s, amount, budget)].into();
    };

    let (head, tail) = match elide {
        Elide::End => (fitting_prefix(s, content, budget), s.len()),
        Elide::Middle => {
            let head = fitting_prefix(s, content.div_ceil(2), budget);
            (head, (s.len() - fitting_suffix(s, content / 2, budget)).max(head))
        }
    };
    format!("{}{marker}{}", &s[..head], &s[tail..]).into()
}

/// Length in bytes of the longest prefix of `s` that costs at most `max`.
fn fitting_prefix(s: &str, max: usize, budget: Budget) -> usize {
    fitting_bytes(s.graphemes(true), max, budget)
}

/// Length in bytes of the longest suffix of `s` that costs at most `max`.
fn fitting_suffix(s: &str, max: usize, budget: Budget) -> usize {
    fitting_bytes(s.graphemes(true).rev(), max, budget)
}

fn fitting_bytes<'a>(
    graphemes: impl Iterator<Item = &'a str>,
    max: usize,
    budget: Budget,
) -> usize {
    let mut used = 0;
    let mut bytes = 0;
    for grapheme in graphemes {
        let cost = budget.cost(grapheme);
        if used + cost > max {
            break;
        }
        used += cost;
        bytes += grapheme.len();
    }
    bytes
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::fits("hello", Budget::Columns(5), Elide::End, "…", "hello")]
    #[case::end("hello world", Budget::Columns(6), Elide::End, "…", "hello…")]
    #[case::middle("hello world", Budget::Columns(6), Elide::Middle, "…", "hel…ld")]
    #[case::ascii_marker("hello world", Budget::Bytes(8), Elide::End, "...", "hello...")]
    #[case::wide_glyphs_cost_two("世界世界", Budget::Columns(5), Elide::End, "…", "世界…")]
    #[case::bytes_never_split_a_char("héllo", Budget::Bytes(5), Elide::End, "…", "h…")]
    #[case::no_room_for_the_marker("hello", Budget::Bytes(2), Elide::End, "...", "he")]
    fn ellipsizes(
        #[case] s: &str,
        #[case] budget: Budget,
        #[case] elide: Elide,
        #[case] marker: &str,
        #[case] expected: &str,
    ) {
        assert_eq!(ellipsize(s, budget, elide, marker), expected);
    }

    proptest! {
        #[test]
        fn never_exceeds_the_budget(s in "\\PC{0,40}", cols in 0usize..30, middle: bool) {
            let elide = if middle { Elide::Middle } else { Elide::End };
            let out = ellipsize(&s, Budget::Columns(cols), elide, "…");
            prop_assert!(out.width() <= cols, "{out:?} is wider than {cols}");
        }
    }
}
