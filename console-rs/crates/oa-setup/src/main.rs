use std::process::ExitCode;

use oa_setup::options::{self, Parsed, USAGE};

fn main() -> ExitCode {
    match options::parse(std::env::args().skip(1)) {
        Ok(Parsed::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Parsed::Run(opts)) => match oa_setup::ui::run(opts) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("open-apollo-setup: {e}");
                ExitCode::FAILURE
            }
        },
        Err(e) => {
            eprintln!("open-apollo-setup: {e}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
