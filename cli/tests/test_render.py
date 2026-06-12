"""The TUI's ANSI sanitizer: colors survive, cursor control dies."""

from mandala_fleet.tui.render import to_text


def test_colors_survive_and_controls_die() -> None:
    line = "\x1b[K\x1b[35;1mwarning:\x1b[0m tree dirty\x1b[K\x07"
    text = to_text(line)
    assert text.plain == "warning: tree dirty"
    assert any(span.style for span in text.spans)  # the SGR color survived


def test_deploy_rs_status_line_renders_plain() -> None:
    line = "🚀 ℹ️ [deploy] [\x1b[38;5;51mINFO\x1b[0m] Activating profile"
    assert to_text(line).plain == "🚀 ℹ️ [deploy] [INFO] Activating profile"
