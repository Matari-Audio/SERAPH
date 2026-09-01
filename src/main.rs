mod capability;
mod kernel;

use std::{env, process::ExitCode};

use anyhow::{Result, bail};
use kernel::Kernel;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let mut args = env::args().skip(1);
    if args.next().as_deref() != Some("exec") {
        bail!("usage: seraph exec '<python cell>' ['<python cell>' ...]");
    }

    let cells: Vec<String> = args.collect();
    if cells.is_empty() {
        bail!("exec requires at least one Python cell");
    }

    let kernel = Kernel::spawn().await?;
    for code in cells {
        let output = kernel.execute(&code).await?;
        print!("{}", output.stdout);
        print!("{}", output.background_stdout);
        eprint!("{}", output.stderr);
        eprint!("{}", output.background_stderr);
        for value in output.emitted {
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        if output.truncated {
            eprintln!("warning: execution output was truncated");
        }
    }

    kernel.shutdown().await
}
