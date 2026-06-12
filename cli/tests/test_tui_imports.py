"""The TUI modules import lazily from the CLI — lock their (module-level)
textual API surface at build time so widget renames fail the package
build, not an operator's session."""


def test_tui_modules_import() -> None:
    import mandala_fleet.tui.deploy  # noqa: F401
    import mandala_fleet.tui.explorer  # noqa: F401
    import mandala_fleet.tui.tasks  # noqa: F401


def test_no_textual_internal_overrides() -> None:
    """Accidentally shadowing a private Widget method (e.g. naming a
    helper `_render`) blanks the whole app at paint time — pin that our
    screens leave textual's internals alone."""
    from textual.screen import ModalScreen, Screen

    from mandala_fleet.tui.deploy import DeployScreen
    from mandala_fleet.tui.tasks import ConfirmScreen, TaskScreen

    for cls, base in ((TaskScreen, Screen), (DeployScreen, Screen), (ConfirmScreen, ModalScreen)):
        for name in ("_render", "render", "_render_content", "render_line", "render_lines"):
            assert getattr(cls, name) is getattr(base, name), f"{cls.__name__}.{name} shadows textual"
