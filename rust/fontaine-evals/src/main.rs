use std::process::ExitCode;

fn main() -> ExitCode {
    ExitCode::from(fontaine_evals::run(std::env::args_os()))
}
