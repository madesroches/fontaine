use std::process::ExitCode;

fn main() -> ExitCode {
    match fontaine::Host::from_process() {
        Ok(host) => fontaine::run(std::env::args_os(), host),
        Err(e) => {
            eprintln!("fontaine: cannot determine the working directory: {e}");
            ExitCode::from(fontaine::EXIT_CONFIG)
        }
    }
}
