use std::process::ExitCode;

/// Debug builds poll the session's futures in frames that come close to the 1 MB main-thread
/// stack Windows gives a process, so the program runs on a thread with room to spare.
const STACK_SIZE: usize = 8 * 1024 * 1024;

fn main() -> ExitCode {
    std::thread::Builder::new()
        .name("main".to_string())
        .stack_size(STACK_SIZE)
        .spawn(run)
        .expect("the OS can start a thread with an 8 MiB stack")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

fn run() -> ExitCode {
    match fontaine::Host::from_process() {
        Ok(host) => fontaine::run(std::env::args_os(), host),
        Err(e) => {
            eprintln!("fontaine: cannot determine the working directory: {e}");
            ExitCode::from(fontaine::EXIT_CONFIG)
        }
    }
}
