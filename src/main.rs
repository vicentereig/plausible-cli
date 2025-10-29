fn main() {
    if let Err(err) = plausible_cli::run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}
