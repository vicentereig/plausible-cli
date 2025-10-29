use crate::{
    client::{
        AggregateQuery, BreakdownQuery, BreakdownResponse, CreateSiteRequest, PlausibleClient,
        RealtimeVisitorsResponse, ResetSiteStatsRequest, SiteSummary, TimeseriesQuery,
        TimeseriesResponse, UpdateSiteRequest,
    },
    config::accounts::{
        AccountExport, AccountProfile, AccountRecord, AccountStore, AccountSummary,
    },
    queue::{JobKind, JobRequest, JobResponse, QueueJobState, QueueSnapshot, Worker, WorkerError},
    rate_limit::{RateLimitConfig, RateLimiter},
    Error,
};
use async_trait::async_trait;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::fs;
use std::io::{self, Read};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use tabled::{Table, Tabled};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

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
    /// Inspect and control the background queue.
    Queue {
        #[command(subcommand)]
        command: QueueCommand,
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
    /// Create a new Plausible site.
    Create(SiteCreateArgs),
    /// Update properties on an existing site.
    Update(SiteUpdateArgs),
    /// Reset statistics for a site.
    Reset(SiteResetArgs),
    /// Delete a site.
    Delete(SiteDeleteArgs),
}

#[derive(Subcommand, Debug, Clone)]
pub enum StatsCommand {
    /// Run an aggregate stats query.
    Aggregate(StatsAggregateArgs),
    /// Run a timeseries stats query.
    Timeseries(StatsTimeseriesArgs),
    /// Run a breakdown stats query.
    Breakdown(StatsBreakdownArgs),
    /// Fetch realtime visitors.
    Realtime(StatsRealtimeArgs),
}

#[derive(Subcommand, Debug, Clone)]
pub enum EventsCommand {
    /// Print an example events payload.
    Template,
    /// Send a single custom event.
    Send(EventSendArgs),
    /// Import newline-delimited events.
    Import(EventImportArgs),
}

#[derive(Subcommand, Debug, Clone)]
pub enum QueueCommand {
    /// Show pending and in-flight jobs.
    Inspect,
    /// Block until the queue is empty.
    Drain,
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
    /// Configure account-specific rate limit budgets.
    Budget(SetBudgetArgs),
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

#[derive(Args, Debug, Clone, Default)]
pub struct SetBudgetArgs {
    /// Account alias to configure.
    #[arg(long)]
    pub alias: String,
    /// Daily request budget (omit or set to zero to clear).
    #[arg(long)]
    pub daily: Option<u32>,
    /// Clear any configured daily budget.
    #[arg(long)]
    pub clear: bool,
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

#[derive(Args, Debug, Default, Clone)]
pub struct SiteCreateArgs {
    /// Domain for the new site.
    #[arg(long)]
    pub domain: String,
    /// Timezone identifier.
    #[arg(long)]
    pub timezone: Option<String>,
    /// Whether the site should be public (provide true/false).
    #[arg(long)]
    pub public: Option<bool>,
}

#[derive(Args, Debug, Default, Clone)]
pub struct SiteUpdateArgs {
    /// Site domain or site_id to update.
    #[arg(long)]
    pub site: String,
    /// Update the timezone.
    #[arg(long)]
    pub timezone: Option<String>,
    /// Update the public flag (true/false).
    #[arg(long)]
    pub public: Option<bool>,
    /// Toggle whether this is the main site (true/false).
    #[arg(long = "main-site")]
    pub main_site: Option<bool>,
}

#[derive(Args, Debug, Default, Clone)]
pub struct SiteResetArgs {
    /// Site domain or site_id to reset.
    #[arg(long)]
    pub site: String,
    /// Optional cut-off date (YYYY-MM-DD).
    #[arg(long)]
    pub date: Option<String>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct SiteDeleteArgs {
    /// Site domain or site_id to delete.
    #[arg(long)]
    pub site: String,
    /// Skip interactive confirmation.
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug, Default, Clone)]
pub struct StatsTimeseriesArgs {
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

#[derive(Args, Debug, Default, Clone)]
pub struct StatsBreakdownArgs {
    /// Site domain or site_id to query.
    #[arg(long)]
    pub site: String,
    /// Property to breakdown by (e.g., event:page, visit:source).
    #[arg(long)]
    pub property: String,
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
    /// Sort parameter for breakdowns.
    #[arg(long)]
    pub sort: Option<String>,
    /// Limit number of rows returned.
    #[arg(long)]
    pub limit: Option<u32>,
    /// Page number (for paginated queries).
    #[arg(long)]
    pub page: Option<u32>,
    /// Include parameter (e.g., previous).
    #[arg(long)]
    pub include: Option<String>,
}

#[derive(Args, Debug, Default, Clone)]
pub struct StatsRealtimeArgs {
    /// Site domain or site_id to query.
    #[arg(long)]
    pub site: String,
}

#[derive(Args, Debug, Clone)]
pub struct EventSendArgs {
    /// Provide a JSON string payload inline.
    #[arg(long)]
    pub data: Option<String>,
    /// Path to a file containing JSON payload.
    #[arg(long)]
    pub file: Option<PathBuf>,
    /// Read payload from STDIN (JSON).
    #[arg(long)]
    pub stdin: bool,
    /// Override the `domain` field in the payload.
    #[arg(long)]
    pub domain: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct EventImportArgs {
    /// Path to a newline-delimited JSON file.
    #[arg(long)]
    pub file: Option<PathBuf>,
    /// Read newline-delimited JSON from STDIN.
    #[arg(long)]
    pub stdin: bool,
    /// Print summary without sending to Plausible.
    #[arg(long)]
    pub dry_run: bool,
    /// Override the `domain` field for each event.
    #[arg(long)]
    pub domain: Option<String>,
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
    let rate_config = RateLimitConfig::default().with_daily_quota(account.daily_budget);
    let rate_limiter = RateLimiter::new(paths.clone(), &account_alias, rate_config).await?;

    let executor = Arc::new(PlausibleExecutor::new(client));
    let queue = Worker::spawn(executor, rate_limiter.clone(), None);

    match &cli.command {
        Commands::Status => {
            render_status(&rate_limiter, cli.output).await?;
        }
        Commands::Sites { command } => match command {
            SitesCommand::List => {
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
            SitesCommand::Create(args) => {
                let request = build_create_site_request(args);
                let ticket = queue
                    .submit(
                        JobRequest {
                            account: account_alias.clone(),
                            kind: JobKind::SiteCreate {
                                request: Box::new(request),
                            },
                        },
                        NonZeroU32::new(1).unwrap(),
                    )
                    .await?;
                let response = ticket.await_result().await?;
                if let JobResponse::SiteCreated(site) = response {
                    render_site_summary(&site, cli.output)?;
                }
            }
            SitesCommand::Update(args) => {
                let (site_id, request) = build_update_site_request(args);
                let ticket = queue
                    .submit(
                        JobRequest {
                            account: account_alias.clone(),
                            kind: JobKind::SiteUpdate {
                                site_id,
                                request: Box::new(request),
                            },
                        },
                        NonZeroU32::new(1).unwrap(),
                    )
                    .await?;
                let response = ticket.await_result().await?;
                if let JobResponse::SiteUpdated(site) = response {
                    render_site_summary(&site, cli.output)?;
                }
            }
            SitesCommand::Reset(args) => {
                let (site_id, request) = build_reset_site_request(args);
                let ticket = queue
                    .submit(
                        JobRequest {
                            account: account_alias.clone(),
                            kind: JobKind::SiteReset {
                                site_id,
                                request: Box::new(request),
                            },
                        },
                        NonZeroU32::new(1).unwrap(),
                    )
                    .await?;
                let response = ticket.await_result().await?;
                if matches!(response, JobResponse::SiteReset | JobResponse::Acknowledged) {
                    println!("Site statistics reset queued successfully.");
                }
            }
            SitesCommand::Delete(args) => {
                if !args.force && !confirm_deletion(&args.site)? {
                    println!("Aborted.");
                } else {
                    let ticket = queue
                        .submit(
                            JobRequest {
                                account: account_alias.clone(),
                                kind: JobKind::SiteDelete {
                                    site_id: args.site.clone(),
                                },
                            },
                            NonZeroU32::new(1).unwrap(),
                        )
                        .await?;
                    let response = ticket.await_result().await?;
                    if matches!(
                        response,
                        JobResponse::SiteDeleted | JobResponse::Acknowledged
                    ) {
                        println!("Site '{}' deleted.", args.site);
                    }
                }
            }
        },
        Commands::Stats { command } => match command {
            StatsCommand::Aggregate(args) => {
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
            StatsCommand::Timeseries(args) => {
                let query = build_timeseries_query(args);
                let ticket = queue
                    .submit(
                        JobRequest {
                            account: account_alias.clone(),
                            kind: JobKind::StatsTimeseries {
                                query: Box::new(query),
                            },
                        },
                        NonZeroU32::new(1).unwrap(),
                    )
                    .await?;
                let response = ticket.await_result().await?;
                if let JobResponse::StatsTimeseries(result) = response {
                    render_timeseries(&result, cli.output)?;
                }
            }
            StatsCommand::Breakdown(args) => {
                let query = build_breakdown_query(args);
                let ticket = queue
                    .submit(
                        JobRequest {
                            account: account_alias.clone(),
                            kind: JobKind::StatsBreakdown {
                                query: Box::new(query),
                            },
                        },
                        NonZeroU32::new(1).unwrap(),
                    )
                    .await?;
                let response = ticket.await_result().await?;
                if let JobResponse::StatsBreakdown(result) = response {
                    render_breakdown(&result, cli.output)?;
                }
            }
            StatsCommand::Realtime(args) => {
                let ticket = queue
                    .submit(
                        JobRequest {
                            account: account_alias.clone(),
                            kind: JobKind::StatsRealtime {
                                site_id: args.site.clone(),
                            },
                        },
                        NonZeroU32::new(1).unwrap(),
                    )
                    .await?;
                let response = ticket.await_result().await?;
                if let JobResponse::StatsRealtime(result) = response {
                    render_realtime(&result, cli.output)?;
                }
            }
        },
        Commands::Events { command } => match command {
            EventsCommand::Template => {
                render_event_template(cli.output)?;
            }
            EventsCommand::Send(args) => {
                let event = load_event_payload(args)?;
                let ticket = queue
                    .submit(
                        JobRequest {
                            account: account_alias.clone(),
                            kind: JobKind::EventSend { event },
                        },
                        NonZeroU32::new(1).unwrap(),
                    )
                    .await?;
                let response = ticket.await_result().await?;
                match response {
                    JobResponse::EventAck | JobResponse::Acknowledged => {
                        println!("Event dispatched to Plausible.");
                    }
                    JobResponse::EventsProcessed { processed } => {
                        println!("Processed batch containing {processed} events.");
                    }
                    JobResponse::Custom(value) => {
                        println!("{}", serde_json::to_string_pretty(&value)?);
                    }
                    other => {
                        println!("Event response: {:?}", other);
                    }
                }
            }
            EventsCommand::Import(args) => {
                let events = load_import_payload(args)?;
                if args.dry_run {
                    let count = events.len();
                    match cli.output {
                        OutputFormat::Human => {
                            println!("Dry run: would import {count} events.");
                        }
                        OutputFormat::Json => {
                            let value = serde_json::json!({
                                "dry_run": true,
                                "count": count,
                            });
                            println!("{}", serde_json::to_string_pretty(&value)?);
                        }
                    }
                } else {
                    let count = events.len();
                    let weight_u32 = u32::try_from(count).map_err(|_| {
                        Error::InvalidInput("too many events for single import batch".into())
                    })?;
                    let ticket = queue
                        .submit(
                            JobRequest {
                                account: account_alias.clone(),
                                kind: JobKind::EventsImport { events },
                            },
                            NonZeroU32::new(weight_u32).unwrap(),
                        )
                        .await?;
                    let response = ticket.await_result().await?;
                    match response {
                        JobResponse::EventsProcessed { processed } => match cli.output {
                            OutputFormat::Human => {
                                println!("Imported {processed} events.");
                            }
                            OutputFormat::Json => {
                                let value = serde_json::json!({ "processed": processed });
                                println!("{}", serde_json::to_string_pretty(&value)?);
                            }
                        },
                        JobResponse::EventAck | JobResponse::Acknowledged => {
                            println!("Import completed.");
                        }
                        JobResponse::Custom(value) => {
                            println!("{}", serde_json::to_string_pretty(&value)?);
                        }
                        other => println!("Import response: {:?}", other),
                    }
                }
            }
        },
        Commands::Queue { command } => match command {
            QueueCommand::Inspect => {
                let snapshot = queue.snapshot().await;
                render_queue_snapshot(&snapshot, cli.output)?;
            }
            QueueCommand::Drain => {
                queue.wait_idle().await;
                match cli.output {
                    OutputFormat::Human => println!("Queue drained."),
                    OutputFormat::Json => {
                        let value = serde_json::json!({ "drained": true });
                        println!("{}", serde_json::to_string_pretty(&value)?);
                    }
                }
            }
        },
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

fn build_timeseries_query(args: &StatsTimeseriesArgs) -> TimeseriesQuery {
    TimeseriesQuery {
        site_id: args.site.clone(),
        metrics: args.metrics.clone(),
        period: args.period.clone(),
        date: args.date.clone(),
        interval: args.interval.clone(),
        filters: args.filters.clone(),
        properties: args.properties.clone(),
        compare: args.compare.clone(),
        sort: args.sort.clone(),
        limit: args.limit,
        page: args.page,
    }
}

fn build_breakdown_query(args: &StatsBreakdownArgs) -> BreakdownQuery {
    BreakdownQuery {
        site_id: args.site.clone(),
        property: args.property.clone(),
        metrics: args.metrics.clone(),
        period: args.period.clone(),
        date: args.date.clone(),
        filters: args.filters.clone(),
        properties: args.properties.clone(),
        compare: args.compare.clone(),
        sort: args.sort.clone(),
        limit: args.limit,
        page: args.page,
        include: args.include.clone(),
    }
}

fn build_create_site_request(args: &SiteCreateArgs) -> CreateSiteRequest {
    CreateSiteRequest {
        domain: args.domain.clone(),
        timezone: args.timezone.clone(),
        public: args.public,
    }
}

fn build_update_site_request(args: &SiteUpdateArgs) -> (String, UpdateSiteRequest) {
    (
        args.site.clone(),
        UpdateSiteRequest {
            timezone: args.timezone.clone(),
            public: args.public,
            main_site: args.main_site,
        },
    )
}

fn build_reset_site_request(args: &SiteResetArgs) -> (String, ResetSiteStatsRequest) {
    (
        args.site.clone(),
        ResetSiteStatsRequest {
            date: args.date.clone(),
        },
    )
}

fn load_event_payload(args: &EventSendArgs) -> Result<serde_json::Value, Error> {
    let sources = args.data.is_some() as u8 + args.file.is_some() as u8 + args.stdin as u8;
    if sources == 0 {
        return Err(Error::InvalidInput(
            "provide one of --data, --file, or --stdin for events send".into(),
        ));
    }
    if sources > 1 {
        return Err(Error::InvalidInput(
            "choose only one of --data, --file, or --stdin for events send".into(),
        ));
    }
    let raw = if let Some(data) = &args.data {
        data.clone()
    } else if let Some(path) = &args.file {
        fs::read_to_string(path)?
    } else {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        buf
    };
    parse_event_json(&raw, &args.domain)
}

fn load_import_payload(args: &EventImportArgs) -> Result<Vec<serde_json::Value>, Error> {
    let sources = args.file.is_some() as u8 + args.stdin as u8;
    if sources == 0 {
        return Err(Error::InvalidInput(
            "provide --file or --stdin for events import".into(),
        ));
    }
    if sources > 1 {
        return Err(Error::InvalidInput(
            "choose either --file or --stdin for events import".into(),
        ));
    }
    let raw = if let Some(path) = &args.file {
        fs::read_to_string(path)?
    } else {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        buf
    };
    parse_ndjson(&raw, &args.domain)
}

fn parse_event_json(contents: &str, domain: &Option<String>) -> Result<serde_json::Value, Error> {
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidInput("event payload is empty".into()));
    }
    let mut value: serde_json::Value = serde_json::from_str(trimmed)?;
    if !value.is_object() {
        return Err(Error::InvalidInput(
            "event payload must be a JSON object".into(),
        ));
    }
    apply_domain_override(&mut value, domain);
    Ok(value)
}

fn parse_ndjson(contents: &str, domain: &Option<String>) -> Result<Vec<serde_json::Value>, Error> {
    let mut events = Vec::new();
    for (idx, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut value: serde_json::Value = serde_json::from_str(trimmed).map_err(|err| {
            Error::InvalidInput(format!("failed to parse line {}: {}", idx + 1, err))
        })?;
        if !value.is_object() {
            return Err(Error::InvalidInput(format!(
                "line {} must be a JSON object",
                idx + 1
            )));
        }
        apply_domain_override(&mut value, domain);
        events.push(value);
    }
    if events.is_empty() {
        return Err(Error::InvalidInput("no events found in input".into()));
    }
    Ok(events)
}

fn apply_domain_override(value: &mut serde_json::Value, domain: &Option<String>) {
    if let (Some(domain), serde_json::Value::Object(map)) = (domain, value) {
        map.insert("domain".into(), serde_json::Value::String(domain.clone()));
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

fn render_site_summary(site: &SiteSummary, format: OutputFormat) -> Result<(), Error> {
    match format {
        OutputFormat::Human => {
            let table = Table::new(vec![SiteRow::from(site)]).to_string();
            println!("{}", table);
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(site)?);
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

fn render_timeseries(response: &TimeseriesResponse, format: OutputFormat) -> Result<(), Error> {
    match format {
        OutputFormat::Human => {
            let rows: Vec<_> = response.results.iter().map(TimeseriesRow::from).collect();
            if rows.is_empty() {
                println!("No timeseries data.");
            } else {
                let table = Table::new(rows).to_string();
                println!("{}", table);
            }
            if !response.totals.is_empty() {
                println!("Totals: {}", format_metrics(&response.totals, &[]));
            }
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
    }
    Ok(())
}

fn render_breakdown(response: &BreakdownResponse, format: OutputFormat) -> Result<(), Error> {
    match format {
        OutputFormat::Human => {
            let rows: Vec<_> = response.results.iter().map(BreakdownRow::from).collect();
            if let Some(page) = response.page {
                if let Some(total) = response.total_pages {
                    println!("Page {}/{}", page, total);
                } else {
                    println!("Page {}", page);
                }
            }
            if rows.is_empty() {
                println!("No breakdown data.");
            } else {
                let table = Table::new(rows).to_string();
                println!("{}", table);
            }
            if let Some(totals) = &response.totals {
                if !totals.is_empty() {
                    println!("Totals: {}", format_metrics(totals, &[]));
                }
            }
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
    }
    Ok(())
}

fn render_realtime(response: &RealtimeVisitorsResponse, format: OutputFormat) -> Result<(), Error> {
    match format {
        OutputFormat::Human => {
            println!("Visitors: {}", response.visitors);
            if let Some(pageviews) = response.pageviews {
                println!("Pageviews: {}", pageviews);
            }
            if let Some(bounce) = response.bounce_rate {
                println!("Bounce rate: {:.2}%", bounce * 100.0);
            }
            if let Some(duration) = response.visit_duration {
                println!("Visit duration: {:.2}s", duration);
            }
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(response)?);
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

fn render_queue_snapshot(snapshot: &[QueueSnapshot], format: OutputFormat) -> Result<(), Error> {
    match format {
        OutputFormat::Human => {
            if snapshot.is_empty() {
                println!("Queue is empty.");
            } else {
                let rows: Vec<_> = snapshot.iter().map(QueueRow::from).collect();
                let table = Table::new(rows).to_string();
                println!("{}", table);
            }
        }
        OutputFormat::Json => {
            let payload: Vec<_> = snapshot
                .iter()
                .map(|entry| {
                    serde_json::json!({
                        "id": entry.id,
                        "account": entry.account,
                        "description": entry.description,
                        "state": match entry.state {
                            QueueJobState::Pending => "pending",
                            QueueJobState::InFlight => "in_flight",
                        },
                        "enqueued_at": format_timestamp(&entry.enqueued_at),
                        "started_at": entry.started_at.as_ref().map(format_timestamp),
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&payload)?);
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
        AccountsCommand::Budget(args) => {
            if args.clear && args.daily.is_some() {
                return Err(Error::InvalidInput(
                    "use either --daily or --clear when configuring budgets".into(),
                ));
            }

            let budget = if args.clear {
                None
            } else if let Some(value) = args.daily {
                if value == 0 {
                    None
                } else {
                    Some(NonZeroU32::new(value).ok_or_else(|| {
                        Error::InvalidInput("daily budget must be greater than zero".into())
                    })?)
                }
            } else {
                return Err(Error::InvalidInput(
                    "provide --daily to set or --clear to remove a budget".into(),
                ));
            };

            store.set_daily_budget(&args.alias, budget)?;
            match budget {
                Some(limit) => println!(
                    "Set daily budget for '{}' to {} requests.",
                    args.alias, limit
                ),
                None => println!("Cleared daily budget for '{}'.", args.alias),
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
                        "daily_budget": account.daily_budget.map(|v| v.get()),
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
struct TimeseriesRow {
    timestamp: String,
    metrics: String,
}

impl From<&serde_json::Map<String, serde_json::Value>> for TimeseriesRow {
    fn from(entry: &serde_json::Map<String, serde_json::Value>) -> Self {
        let timestamp = entry
            .get("datetime")
            .or_else(|| entry.get("date"))
            .or_else(|| entry.get("time"))
            .map(format_value)
            .unwrap_or_else(|| "n/a".into());
        let metrics = format_metrics(entry, &["datetime", "date", "time"]);
        Self { timestamp, metrics }
    }
}

#[derive(Tabled)]
struct BreakdownRow {
    segment: String,
    metrics: String,
}

impl From<&serde_json::Map<String, serde_json::Value>> for BreakdownRow {
    fn from(entry: &serde_json::Map<String, serde_json::Value>) -> Self {
        let segment = entry
            .get("value")
            .or_else(|| entry.get("name"))
            .map(format_value)
            .unwrap_or_else(|| "n/a".into());
        let metrics = format_metrics(entry, &["value", "name"]);
        Self { segment, metrics }
    }
}

#[derive(Tabled)]
struct QueueRow {
    id: u64,
    account: String,
    description: String,
    state: String,
    enqueued_at: String,
    started_at: String,
}

impl From<&QueueSnapshot> for QueueRow {
    fn from(entry: &QueueSnapshot) -> Self {
        Self {
            id: entry.id,
            account: entry.account.clone(),
            description: entry.description.clone(),
            state: match entry.state {
                QueueJobState::Pending => "pending".into(),
                QueueJobState::InFlight => "in-flight".into(),
            },
            enqueued_at: format_timestamp(&entry.enqueued_at),
            started_at: entry
                .started_at
                .as_ref()
                .map(format_timestamp)
                .unwrap_or_else(|| "n/a".into()),
        }
    }
}

fn confirm_deletion(site: &str) -> Result<bool, Error> {
    println!("Delete site '{}'? Type 'yes' to confirm:", site);
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim().to_lowercase().as_str(), "yes" | "y"))
}

fn format_metrics(map: &serde_json::Map<String, serde_json::Value>, skip: &[&str]) -> String {
    map.iter()
        .filter(|(key, _)| {
            let key_str = key.as_str();
            !skip.contains(&key_str)
        })
        .map(|(key, value)| format!("{key}={}", format_value(value)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".into(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "<unserializable>".into()),
    }
}

fn format_timestamp(timestamp: &OffsetDateTime) -> String {
    timestamp
        .format(&Rfc3339)
        .unwrap_or_else(|_| timestamp.to_string())
}

#[derive(Tabled)]
struct AccountRow {
    alias: String,
    label: String,
    email: String,
    default: String,
    daily_budget: String,
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
            daily_budget: summary
                .daily_budget
                .map(|v| v.get().to_string())
                .unwrap_or_else(|| "-".into()),
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
    daily_budget: String,
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
            daily_budget: export
                .daily_budget
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".into()),
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
            JobKind::StatsTimeseries { query } => {
                let result = self
                    .client
                    .stats_timeseries(&query)
                    .await
                    .map_err(|err| WorkerError::Execution(err.to_string()))?;
                Ok(JobResponse::StatsTimeseries(result))
            }
            JobKind::StatsBreakdown { query } => {
                let result = self
                    .client
                    .stats_breakdown(&query)
                    .await
                    .map_err(|err| WorkerError::Execution(err.to_string()))?;
                Ok(JobResponse::StatsBreakdown(result))
            }
            JobKind::SiteCreate { request } => {
                let site = self
                    .client
                    .create_site(&request)
                    .await
                    .map_err(|err| WorkerError::Execution(err.to_string()))?;
                Ok(JobResponse::SiteCreated(site))
            }
            JobKind::SiteUpdate { site_id, request } => {
                let site = self
                    .client
                    .update_site(&site_id, &request)
                    .await
                    .map_err(|err| WorkerError::Execution(err.to_string()))?;
                Ok(JobResponse::SiteUpdated(site))
            }
            JobKind::SiteReset { site_id, request } => {
                self.client
                    .reset_site_stats(&site_id, &request)
                    .await
                    .map_err(|err| WorkerError::Execution(err.to_string()))?;
                Ok(JobResponse::SiteReset)
            }
            JobKind::SiteDelete { site_id } => {
                self.client
                    .delete_site(&site_id)
                    .await
                    .map_err(|err| WorkerError::Execution(err.to_string()))?;
                Ok(JobResponse::SiteDeleted)
            }
            JobKind::StatsRealtime { site_id } => {
                let realtime = self
                    .client
                    .stats_realtime_visitors(&site_id)
                    .await
                    .map_err(|err| WorkerError::Execution(err.to_string()))?;
                Ok(JobResponse::StatsRealtime(realtime))
            }
            JobKind::EventSend { event } => {
                self.client
                    .send_event(&event)
                    .await
                    .map_err(|err| WorkerError::Execution(err.to_string()))?;
                Ok(JobResponse::EventAck)
            }
            JobKind::EventsImport { events } => {
                let mut processed = 0usize;
                for event in events {
                    self.client
                        .send_event(&event)
                        .await
                        .map_err(|err| WorkerError::Execution(err.to_string()))?;
                    processed += 1;
                }
                Ok(JobResponse::EventsProcessed { processed })
            }
            JobKind::Custom { .. } => Ok(JobResponse::Acknowledged),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeseries_builder_copies_fields() {
        let args = StatsTimeseriesArgs {
            site: "example.com".into(),
            metrics: vec!["visitors".into()],
            period: Some("7d".into()),
            date: Some("2024-01-01,2024-01-07".into()),
            filters: vec!["event:page==/docs".into()],
            properties: vec!["visit:source".into()],
            compare: Some("previous_period".into()),
            interval: Some("date".into()),
            sort: Some("visitors:desc".into()),
            limit: Some(10),
            page: Some(2),
        };

        let query = build_timeseries_query(&args);
        assert_eq!(query.site_id, "example.com");
        assert_eq!(query.metrics, vec!["visitors"]);
        assert_eq!(query.period.as_deref(), Some("7d"));
        assert_eq!(query.interval.as_deref(), Some("date"));
        assert_eq!(query.limit, Some(10));
        assert_eq!(query.page, Some(2));
        assert_eq!(query.filters, vec!["event:page==/docs"]);
        assert_eq!(query.properties, vec!["visit:source"]);
    }

    #[test]
    fn breakdown_builder_includes_property_and_include() {
        let args = StatsBreakdownArgs {
            site: "example.com".into(),
            property: "event:page".into(),
            metrics: vec!["visitors".into(), "pageviews".into()],
            period: Some("30d".into()),
            date: None,
            filters: vec![],
            properties: vec![],
            compare: Some("previous_period".into()),
            sort: Some("visitors:desc".into()),
            limit: Some(25),
            page: Some(1),
            include: Some("previous".into()),
        };

        let query = build_breakdown_query(&args);
        assert_eq!(query.property, "event:page");
        assert_eq!(query.metrics, vec!["visitors", "pageviews"]);
        assert_eq!(query.include.as_deref(), Some("previous"));
        assert_eq!(query.limit, Some(25));
    }

    #[test]
    fn parse_event_json_applies_domain_override() {
        let domain = Some(String::from("example.com"));
        let value = parse_event_json(r#"{"name":"Signup"}"#, &domain).expect("parse");
        assert_eq!(
            value.get("domain").and_then(|v| v.as_str()),
            Some("example.com")
        );
    }

    #[test]
    fn parse_ndjson_parses_multiple_events() {
        let domain = None;
        let events = parse_ndjson("{\"name\":\"Signup\"}\n{\"name\":\"Upgrade\"}\n", &domain)
            .expect("ndjson");
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].get("name").and_then(|v| v.as_str()),
            Some("Signup")
        );
    }

    #[test]
    fn parse_ndjson_errors_when_empty() {
        let err = parse_ndjson("\n\n", &None).expect_err("empty");
        match err {
            Error::InvalidInput(msg) => assert!(msg.contains("no events")),
            _ => panic!("unexpected error"),
        }
    }

    #[test]
    fn build_create_site_request_sets_optional_fields() {
        let args = SiteCreateArgs {
            domain: "example.com".into(),
            timezone: Some("UTC".into()),
            public: Some(true),
        };
        let request = build_create_site_request(&args);
        assert_eq!(request.domain, "example.com");
        assert_eq!(request.timezone.as_deref(), Some("UTC"));
        assert_eq!(request.public, Some(true));
    }

    #[test]
    fn build_update_site_request_respects_options() {
        let args = SiteUpdateArgs {
            site: "example.com".into(),
            timezone: Some("Europe/Berlin".into()),
            public: Some(false),
            main_site: Some(true),
        };
        let (site_id, request) = build_update_site_request(&args);
        assert_eq!(site_id, "example.com");
        assert_eq!(request.timezone.as_deref(), Some("Europe/Berlin"));
        assert_eq!(request.public, Some(false));
        assert_eq!(request.main_site, Some(true));
    }

    #[test]
    fn build_reset_site_request_handles_optional_date() {
        let args = SiteResetArgs {
            site: "example.com".into(),
            date: Some("2024-01-01".into()),
        };
        let (site_id, request) = build_reset_site_request(&args);
        assert_eq!(site_id, "example.com");
        assert_eq!(request.date.as_deref(), Some("2024-01-01"));
    }
}
