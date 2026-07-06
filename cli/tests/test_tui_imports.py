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
    from textual.widgets import DataTable

    from mandala_fleet.tui.deploy import DeployScreen
    from mandala_fleet.tui.select_table import SelectTable
    from mandala_fleet.tui.tasks import ConfirmScreen, RebootScreen, TaskScreen

    for cls, base in (
        (TaskScreen, Screen),
        (DeployScreen, Screen),
        (ConfirmScreen, ModalScreen),
        (RebootScreen, ModalScreen),
        (SelectTable, DataTable),
    ):
        for name in ("_render", "render", "_render_content", "render_line", "render_lines"):
            assert getattr(cls, name) is getattr(base, name), f"{cls.__name__}.{name} shadows textual"


def test_mcp_panel_toggle_binding() -> None:
    """The activity pane is hidable: the `m` binding + action exist, and
    the binding is hidden (check_action → None) when no MCP host runs."""
    from mandala_fleet.tui.explorer import ExplorerApp

    assert any(b.key == "m" and b.action == "toggle_mcp" for b in ExplorerApp.BINDINGS)
    app = ExplorerApp.__new__(ExplorerApp)  # no App.__init__: header-only check
    app._serve_mcp = False
    assert app.check_action("toggle_mcp", ()) is None
    app._serve_mcp = True
    assert app.check_action("toggle_mcp", ()) is True
