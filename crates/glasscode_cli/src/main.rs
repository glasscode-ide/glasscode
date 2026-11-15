//! `glasscode-cli`: Glasscode's console program. On Windows the app is a window program, which
//! cannot write to a console, so the `glasscode` command is this program: the app copies it to
//! `~/.glasscode/bin/glasscode.exe` and puts that folder on the PATH of each session. On the Mac
//! the app's own binary does the same work (`Glasscode cli …`).

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // A mode for other programs (`browser-mcp`, `relay-connect`); any other words are a command
    // for the app.
    let mode = args.split_first().and_then(|(mode, rest)| glasscode_core::cli::run_mode(mode, rest));
    std::process::exit(mode.unwrap_or_else(|| glasscode_core::cli::main(args)));
}
