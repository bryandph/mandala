fn main() {
    let Some(run_dir) = std::env::args_os().nth(1) else {
        eprintln!("usage: mandala-run-supervisor <run-dir>");
        std::process::exit(2);
    };
    let code = match mandala_core::runner::run_deploy_supervisor(std::path::Path::new(&run_dir)) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("mandala deploy supervisor: {error}");
            1
        }
    };
    std::process::exit(u8::try_from(code).unwrap_or(1).into());
}
