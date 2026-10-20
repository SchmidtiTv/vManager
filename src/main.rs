//! Binary entry point. `vman` (`src/bin/vman.rs`) is the same program under
//! a shorter name; both hand over to [`vmanager::main`].

use std::process::ExitCode;

fn main() -> ExitCode {
    vmanager::main()
}
