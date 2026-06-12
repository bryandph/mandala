"""The TUI modules import lazily from the CLI — lock their (module-level)
textual API surface at build time so widget renames fail the package
build, not an operator's session."""


def test_tui_modules_import() -> None:
    import mandala_fleet.tui.deploy  # noqa: F401
    import mandala_fleet.tui.explorer  # noqa: F401
