use crate::{
    client::{AggregateQuery, PlausibleClient, SiteSummary},
    config::accounts::{
        AccountExport, AccountProfile, AccountRecord, AccountStore, AccountSummary,
    },
    queue::{JobKind, JobRequest, JobResponse, Worker, WorkerError},
    rate_limit::{RateLimitConfig, RateLimiter},
    Error,
};
use async_trait::async_trait;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::num::NonZeroU32;
use std::sync::Arc;
use tabled::{Table, Tabled};
use time::OffsetDateTime;

use crate::config::ConfigPaths;
#[derive(Parser, Debug)]
#[command(name = "plausible", version = env!("CARGO_PKG_VERSION"), arg_required_else_help = true)]
pub struct Cli {
    /// Override the account alias to use for this invocation.
    #[arg(long, short = 'A', global = true)]
    pub account: Option<String>,

    /// Output format for command results.
    #[arg(long, value_enum, global = true, default_value_t = OutputFormat::Human)]
    pub output: OutputFormat,

    /// Custom Plausible base URL (needed for self-hosted instances).
    #[arg(long, global = true)]
    pub base_url: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
    /// Display current rate-limit usage.
    Status,
    /// Operations for Plausible sites.
    Sites {
        #[command(subcommand)]
        command: SitesCommand,
    },
    /// Fetch Plausible statistics.
    Stats {
        #[command(subcommand)]
        command: StatsCommand,
    },
    /// Manage or send custom events.
    Events {
        #[command(subcommand)]
        command: EventsCommand,
    },
    /// Manage stored Plausible accounts.
    Accounts {
        #[command(subcommand)]
        command: AccountsCommand,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum SitesCommand {
    /// List all sites visible to the current account.
    List,
}

#[derive(Subcommand, Debug, Clone)]
pub enum StatsCommand {
    /// Run an aggregate stats query.
    Aggregate(StatsAggregateArgs),
}

#[derive(Subcommand, Debug, Clone)]
pub enum EventsCommand {
    /// Print an example events payload.
    Template,
}

#[derive(Subcommand, Debug, Clone)]
pub enum AccountsCommand {
    /// List configured accounts.
    List,
    /// Add a new account alias.
    Add(AddAccountArgs),
    /// Set an existing account as default.
    Use { alias: String },
    /// Remove an account alias and its credentials.
    Remove { alias: String },
    /// Export account metadata without secrets.
    Export {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args, Debug, Clone, Default)]
pub struct AddAccountArgs {
    /// Alias used to reference this account.
    #[arg(long)]
    pub alias: String,
    /// API key associated with the Plausible account.
    #[arg(long, env = "PLAUSIBLE_API_KEY")]
    pub api_key: String,
    /// Optional friendly label.
    #[arg(long)]
    pub label: Option<String>,
    /// Optional contact email.
    #[arg(long)]
    pub email: Option<String>,
    /// Optional description for humans/LLMs.
    #[arg(long)]
    pub description: Option<String>,
}

#[derive(Args, Debug, Default, Clone)]
pub struct StatsAggregateArgs {
    /// Site domain or site_id to query.
    #[arg(long)]
    pub site: String,
    /// Metrics to include (repeatable).
    #[arg(long = "metric", short = 'm')]
    pub metrics: Vec<String>,
    /// Period string (e.g., 7d, 30d, month, custom).
    #[arg(long)]
    pub period: Option<String>,
    /// Custom date range (YYYY-MM-DD,YYYY-MM-DD).
    #[arg(long)]
    pub date: Option<String>,
    /// Filters to apply (repeatable, Plausible filter syntax).
    #[arg(long)]
    pub filters: Vec<String>,
    /// Event properties to include (repeatable).
    #[arg(long)]
    pub properties: Vec<String>,
    /// Compare parameter (e.g., previous_period).
    #[arg(long)]
    pub compare: Option<String>,
    /// Interval parameter (e.g., date, week).
    #[arg(long)]
    pub interval: Option<String>,
    /// Sort parameter for breakdowns.
    #[arg(long)]
    pub sort: Option<String>,
    /// Limit number of rows returned.
    #[arg(long)]
    pub limit: Option<u32>,
    /// Page number (for paginated queries).
    #[arg(long)]
    pub page: Option<u32>,
}

#[derive(Copy, Clone, Debug, ValueEnum, Default)]
pub enum OutputFormat {
    #[default]
    Human,
    Json,
}

pub async fn execute(cli: Cli) -> Result<(), Error> {
    let paths = ConfigPaths::with_project_dirs()?;
    let account_store = AccountStore::new(paths.clone())?;

    if let Commands::Accounts { command } = &cli.command {
        handle_account_command(&account_store, command, cli.output)?;
        return Ok(());
    }

    let account_alias = resolve_account(&account_store, &cli.account)?;
    let account = account_store.get_account(&account_alias)?;
    let client = build_client(&cli, &account)?;
    let rate_limiter =
        RateLimiter::new(paths.clone(), &account_alias, RateLimitConfig::default()).await?;

    let executor = Arc::new(PlausibleExecutor::new(client));
    let queue = Worker::spawn(executor, rate_limiter.clone(), None);

    match &cli.command {
        Commands::Status => {
            render_status(&rate_limiter, cli.output).await?;
        }
        Commands::Sites {
            command: SitesCommand::List,
        } => {
            let ticket = queue
                .submit(
                    JobRequest {
                        account: account_alias.clone(),
                        kind: JobKind::ListSites,
                    },
                    NonZeroU32::new(1).unwrap(),
                )
                .await?;
            let response = ticket.await_result().await?;
            if let JobResponse::Sites(sites) = response {
                render_sites(&sites, cli.output)?;
            }
        }
        Commands::Stats {
            command: StatsCommand::Aggregate(args),
        } => {
            let query = build_aggregate_query(args);
            let ticket = queue
                .submit(
                    JobRequest {
                        account: account_alias.clone(),
                        kind: JobKind::StatsAggregate {
                            query: Box::new(query),
                        },
                    },
                    NonZeroU32::new(1).unwrap(),
                )
                .await?;
            let response = ticket.await_result().await?;
            if let JobResponse::StatsAggregate(result) = response {
                render_aggregate(&result, cli.output)?;
            }
        }
        Commands::Events {
            command: EventsCommand::Template,
        } => {
            render_event_template(cli.output)?;
        }
        Commands::Accounts { .. } => unreachable!(),
    }

    Ok(())
}

fn build_client(cli: &Cli, account: &AccountRecord) -> Result<PlausibleClient, Error> {
    if let Some(base) = &cli.base_url {
        let url = url::Url::parse(base).map_err(crate::client::ClientError::InvalidBaseUrl)?;
        Ok(PlausibleClient::with_base_url(
            account.api_key.clone(),
            url,
        )?)
    } else {
        Ok(PlausibleClient::new(account.api_key.clone())?)
    }
}

fn resolve_account(store: &AccountStore, override_alias: &Option<String>) -> Result<String, Error> {
    if let Some(alias) = override_alias {
        return Ok(alias.clone());
    }
    match store.default_alias()? {
        Some(alias) => Ok(alias),
        None => Err(Error::NoDefaultAccount),
    }
}

fn build_aggregate_query(args: &StatsAggregateArgs) -> AggregateQuery {
    AggregateQuery {
        site_id: args.site.clone(),
        metrics: args.metrics.clone(),
        period: args.period.clone(),
        date: args.date.clone(),
        filters: args.filters.clone(),
        properties: args.properties.clone(),
        compare: args.compare.clone(),
        interval: args.interval.clone(),
        sort: args.sort.clone(),
        limit: args.limit,
        page: args.page,
    }
}

async fn render_status(rate_limiter: &RateLimiter, format: OutputFormat) -> Result<(), Error> {
    let status = rate_limiter.status(OffsetDateTime::now_utc()).await?;
    match format {
        OutputFormat::Human => {
            println!(
                "Hourly usage: {}/{} (resets at {})",
                status.hourly_used, status.hourly_limit, status.hourly_reset_at
            );
            if let Some(limit) = status.daily_limit {
                let used = status.daily_used.unwrap_or(0);
                let remaining = status
                    .daily_remaining
                    .unwrap_or_else(|| limit.saturating_sub(used));
                if let Some(reset) = status.daily_reset_at {
                    println!(
                        "Daily usage: {}/{limit} (remaining {remaining}) – resets at {}",
                        used, reset
                    );
                } else {
                    println!("Daily usage: {}/{limit} (remaining {remaining})", used);
                }
            }
        }
        OutputFormat::Json => {
            let value = serde_json::json!({
                "hourly": {
                    "used": status.hourly_used,
                    "limit": status.hourly_limit,
                    "remaining": status.hourly_remaining,
                    "reset_at": status.hourly_reset_at,
                },
                "daily": {
                    "used": status.daily_used,
                    "limit": status.daily_limit,
                    "remaining": status.daily_remaining,
                    "reset_at": status.daily_reset_at,
                }
            });
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
    }
    Ok(())
}

fn render_sites(sites: &[SiteSummary], format: OutputFormat) -> Result<(), Error> {
    match format {
        OutputFormat::Human => {
            let rows: Vec<_> = sites.iter().map(SiteRow::from).collect();
            let table = Table::new(rows).to_string();
            println!("{}", table);
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&sites)?);
        }
    }
    Ok(())
}

fn render_aggregate(
    response: &crate::client::AggregateResponse,
    format: OutputFormat,
) -> Result<(), Error> {
    match format {
        OutputFormat::Human => {
            let mut rows = Vec::new();
            for (metric, value) in &response.results {
                let formatted = match value {
                    serde_json::Value::Number(num) => num.to_string(),
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                rows.push(MetricRow {
                    metric: metric.clone(),
                    value: formatted,
                });
            }
            let table = Table::new(rows).to_string();
            println!("{}", table);
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
    }
    Ok(())
}

fn render_event_template(format: OutputFormat) -> Result<(), Error> {
    let sample = serde_json::json!({
        "name": "Signup",
        "url": "https://example.com/signup",
        "domain": "example.com",
        "referrer": "https://google.com",
        "utm_source": "newsletter",
        "utm_medium": "email",
        "device_type": "desktop"
    });
    match format {
        OutputFormat::Human => {
            println!(
                "Sample event payload:\n{}",
                serde_json::to_string_pretty(&sample)?
            );
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&sample)?);
        }
    }
    Ok(())
}

fn handle_account_command(
    store: &AccountStore,
    command: &AccountsCommand,
    format: OutputFormat,
) -> Result<(), Error> {
    match command {
        AccountsCommand::List => {
            let accounts = store.list_accounts()?;
            render_account_list(&accounts, format)?;
        }
        AccountsCommand::Add(args) => {
            store.add_account(
                &args.alias,
                &args.api_key,
                AccountProfile {
                    label: args.label.clone(),
                    email: args.email.clone(),
                    description: args.description.clone(),
                },
            )?;
            println!("Added account '{}'.", args.alias);
        }
        AccountsCommand::Use { alias } => {
            store.set_default(alias)?;
            println!("Set '{}' as the default account.", alias);
        }
        AccountsCommand::Remove { alias } => {
            store.remove_account(alias)?;
            println!("Removed account '{}'.", alias);
        }
        AccountsCommand::Export { json } => {
            let exports = store.export_accounts()?;
            if *json || matches!(format, OutputFormat::Json) {
                println!("{}", serde_json::to_string_pretty(&exports)?);
            } else {
                render_account_export(&exports)?;
            }
        }
    }
    Ok(())
}

fn render_account_list(accounts: &[AccountSummary], format: OutputFormat) -> Result<(), Error> {
    match format {
        OutputFormat::Human => {
            let rows: Vec<_> = accounts.iter().map(AccountRow::from).collect();
            let table = Table::new(rows).to_string();
            println!("{}", table);
        }
        OutputFormat::Json => {
            let payload: Vec<_> = accounts
                .iter()
                .map(|account| {
                    serde_json::json!({
                        "alias": account.alias,
                        "label": account.profile.label,
                        "email": account.profile.email,
                        "description": account.profile.description,
                        "default": account.is_default,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
    }
    Ok(())
}

fn render_account_export(exports: &[AccountExport]) -> Result<(), Error> {
    let rows: Vec<_> = exports.iter().map(ExportRow::from).collect();
    let table = Table::new(rows).to_string();
    println!("{}", table);
    Ok(())
}

#[derive(Tabled)]
struct SiteRow {
    domain: String,
    timezone: String,
    public: String,
    verified: String,
}

impl From<&SiteSummary> for SiteRow {
    fn from(site: &SiteSummary) -> Self {
        Self {
            domain: site.domain.clone(),
            timezone: site.timezone.clone().unwrap_or_else(|| "n/a".into()),
            public: site
                .public
                .map(|v| v.to_string())
                .unwrap_or_else(|| "n/a".into()),
            verified: site
                .verified
                .map(|v| v.to_string())
                .unwrap_or_else(|| "n/a".into()),
        }
    }
}

#[derive(Tabled)]
struct MetricRow {
    metric: String,
    value: String,
}

#[derive(Tabled)]
struct AccountRow {
    alias: String,
    label: String,
    email: String,
    default: String,
}

impl From<&AccountSummary> for AccountRow {
    fn from(summary: &AccountSummary) -> Self {
        Self {
            alias: summary.alias.clone(),
            label: summary
                .profile
                .label
                .clone()
                .unwrap_or_else(|| "n/a".into()),
            email: summary
                .profile
                .email
                .clone()
                .unwrap_or_else(|| "n/a".into()),
            default: if summary.is_default {
                "yes".into()
            } else {
                "no".into()
            },
        }
    }
}

#[derive(Tabled)]
struct ExportRow {
    alias: String,
    label: String,
    email: String,
    default: String,
    description: String,
}

impl From<&AccountExport> for ExportRow {
    fn from(export: &AccountExport) -> Self {
        Self {
            alias: export.alias.clone(),
            label: export.label.clone().unwrap_or_else(|| "n/a".into()),
            email: export.email.clone().unwrap_or_else(|| "n/a".into()),
            default: if export.is_default {
                "yes".into()
            } else {
                "no".into()
            },
            description: export.description.clone().unwrap_or_else(|| "".into()),
        }
    }
}

struct PlausibleExecutor {
    client: PlausibleClient,
}

impl PlausibleExecutor {
    fn new(client: PlausibleClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl crate::queue::JobExecutor for PlausibleExecutor {
    async fn execute(&self, request: JobRequest) -> crate::queue::JobResult {
        match request.kind {
            JobKind::ListSites => {
                let sites = self
                    .client
                    .list_sites()
                    .await
                    .map_err(|err| WorkerError::Execution(err.to_string()))?;
                Ok(JobResponse::Sites(sites))
            }
            JobKind::StatsAggregate { query } => {
                let result = self
                    .client
                    .stats_aggregate(&query)
                    .await
                    .map_err(|err| WorkerError::Execution(err.to_string()))?;
                Ok(JobResponse::StatsAggregate(result))
            }
            JobKind::Custom { .. } => Ok(JobResponse::Acknowledged),
        }
    }
}
