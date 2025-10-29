use clap::Parser;

pub mod cli;
pub mod client;
pub mod config;
pub mod queue;
pub mod rate_limit;

/// Entry point used by the binary crate.
pub fn run() -> Result<(), crate::Error> {
    let cli = cli::Cli::parse();
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(cli::execute(cli))
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    Accounts(#[from] config::accounts::AccountStoreError),
    #[error(transparent)]
    RateLimit(#[from] rate_limit::RateLimitError),
    #[error(transparent)]
    Client(#[from] client::ClientError),
    #[error(transparent)]
    Worker(#[from] queue::WorkerError),
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
    #[error(transparent)]
    Runtime(#[from] std::io::Error),
    #[error("no default account configured; add an account or pass --account")]
    NoDefaultAccount,
}
