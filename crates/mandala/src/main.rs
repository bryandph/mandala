//! mandala — the fleet porcelain binary (CLI + stdio MCP server).
//!
//! Phase-1 scaffold (OpenSpec change `mandala-rust-rewrite`). The clap CLI
//! lands in section 3; for now the binary prints its banner and dispatches
//! `mandala mcp` to the section-1.2 stdio-MCP spike.

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("mcp") => {
            if let Err(err) = mandala_mcp::serve_stdio_blocking() {
                eprintln!("mandala mcp: {err}");
                std::process::exit(1);
            }
        }
        _ => {
            println!("{}", mandala_core::banner());
            println!("{}", mandala_mcp::server_name());
        }
    }
}
