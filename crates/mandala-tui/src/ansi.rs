//! Shared ANSI render helper for the task/deploy log panes — the
//! `tui/render.py` port (task 5.3).
//!
//! deploy-rs / nix / ansible output carries cursor-control CSI sequences
//! (erase-line `ESC[K`, cursor moves) besides SGR colors. Keep the colors,
//! drop everything else — rendered raw they shred the panes. The pre-filter
//! is a scanner implementing exactly the two `render.py` regexes:
//!
//! * `_CSI_RE = \x1b\[[0-9;?]*[ -/]*[@-~]` — a matched sequence is KEPT only
//!   when its final byte is `m` (SGR), otherwise dropped.
//! * `_CTRL_RE = [\x00-\x08\x0b-\x1a\x1c-\x1f\x7f]` — C0 controls stripped,
//!   except ESC (SGR must survive for the converter), tab, and newline.
//!
//! The surviving SGR is converted to ratatui spans by `ansi-to-tui` (the
//! design's cherry-picked crate; chosen over a hand-rolled SGR parser
//! because 8.x targets the same ratatui-core 0.1 as ratatui 0.30, and the
//! crate covers indexed/truecolor SGR the deploy streams occasionally
//! carry — `rich_style` stays the CORE-vocabulary mapper, a different job).

use ansi_to_tui::IntoText;
use ratatui::text::Line;

/// The `render.py` pre-filter: keep SGR CSI, drop all other CSI, strip C0
/// controls except ESC/tab/newline. Byte-exact port of the two regexes.
#[must_use]
pub fn filter_ansi(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // CSI: ESC `[` params `[0-9;?]*` intermediates `[ -/]*` final `[@-~]`.
            let mut j = i + 2;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || matches!(bytes[j], b';' | b'?'))
            {
                j += 1;
            }
            while j < bytes.len() && (0x20..=0x2f).contains(&bytes[j]) {
                j += 1;
            }
            if j < bytes.len() && (0x40..=0x7e).contains(&bytes[j]) {
                if bytes[j] == b'm' {
                    out.extend_from_slice(&bytes[i..=j]); // SGR survives
                }
                i = j + 1;
                continue;
            }
            // Incomplete CSI: the regex would not match — ESC falls through
            // (it is exempt from the C0 strip) and the rest is kept verbatim.
        }
        // C0 strip: 00-08, 0b-1a, 1c-1f, 7f (keeps \t 09, \n 0a, ESC 1b).
        if matches!(c, 0x00..=0x08 | 0x0b..=0x1a | 0x1c..=0x1f | 0x7f) {
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    // Only whole ASCII bytes were removed, so UTF-8 sequences are intact.
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// One raw output line → a styled ratatui [`Line`]: pre-filter, then SGR
/// conversion. Every streamed line in the task/attached/deploy panes goes
/// through here (the `to_text` analog).
#[must_use]
pub fn to_line(line: &str) -> Line<'static> {
    let cleaned = filter_ansi(line);
    match cleaned.clone().into_bytes().into_text() {
        Ok(text) => text.lines.into_iter().next().unwrap_or_default(),
        // Unconvertible bytes: fall back to the cleaned plain text (never
        // lose the line — the pane is a diagnostic surface).
        Err(_) => Line::from(cleaned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier};

    #[test]
    fn sgr_is_kept_and_other_csi_dropped() {
        // Erase-line + cursor-column dropped, SGR kept (the render.py doc case).
        assert_eq!(
            filter_ansi("\x1b[2K\x1b[1G\x1b[31mred\x1b[0m ok"),
            "\x1b[31mred\x1b[0m ok"
        );
        // Private-mode (cursor hide) and cursor-move sequences vanish.
        assert_eq!(filter_ansi("\x1b[?25la\x1b[10;20Hb"), "ab");
        // Multi-param SGR survives whole.
        assert_eq!(filter_ansi("\x1b[1;32mgo\x1b[m"), "\x1b[1;32mgo\x1b[m");
    }

    #[test]
    fn c0_controls_stripped_except_esc_tab_newline() {
        assert_eq!(filter_ansi("a\x07b\tc\rd"), "ab\tcd");
        assert_eq!(filter_ansi("x\x7fy\x00z"), "xyz");
        // A bare ESC (not starting a CSI) survives, exactly like the Python
        // regex pair (ESC is exempt from _CTRL_RE).
        assert_eq!(filter_ansi("a\x1bb"), "a\x1bb");
        // Newlines survive (outside both regex character classes).
        assert_eq!(filter_ansi("a\nb"), "a\nb");
    }

    #[test]
    fn incomplete_csi_is_left_verbatim() {
        // No final byte → the regex would not match; nothing is eaten.
        assert_eq!(filter_ansi("tail: \x1b[12;"), "tail: \x1b[12;");
    }

    #[test]
    fn to_line_converts_sgr_to_spans() {
        let line = to_line("\x1b[2K\x1b[31mfailed\x1b[0m: host x");
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "failed: host x");
        let red = line
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "failed")
            .expect("styled span");
        assert_eq!(red.style.fg, Some(Color::Red));
    }

    #[test]
    fn to_line_bold_and_plain() {
        let line = to_line("\x1b[1mPLAY RECAP\x1b[0m *****");
        let bold = &line.spans[0];
        assert_eq!(bold.content.as_ref(), "PLAY RECAP");
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        let plain = to_line("no escapes at all");
        let text: String = plain.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "no escapes at all");
    }
}
