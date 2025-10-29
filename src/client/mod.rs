use regex::escape;
use reqwest::{Client as HttpClient, Response, StatusCode};
use serde::{de::DeserializeOwned, ser::SerializeSeq, Deserialize, Serialize, Serializer};
use serde_json::{Map, Number, Value};
use std::time::Duration;
use time::{format_description::well_known::Rfc3339, Duration as TimeDuration, OffsetDateTime};
use url::Url;

const DEFAULT_BASE_URL: &str = "https://plausible.io/";
const DEFAULT_TIMEOUT_SECS: u64 = 15;

/// High-level Plausible API client.
#[derive(Debug, Clone)]
pub struct PlausibleClient {
    http: HttpClient,
    base_url: Url,
    api_key: String,
    user_agent: String,
}

impl PlausibleClient {
    const REALTIME_WINDOW_MINUTES: i64 = 5;
    /// Create a new client targeting the public Plausible API endpoint.
    pub fn new(api_key: impl Into<String>) -> Result<Self, ClientError> {
        let base = Url::parse(DEFAULT_BASE_URL).map_err(ClientError::InvalidBaseUrl)?;
        Self::with_base_url(api_key, base)
    }

    /// Create a client with a custom base URL (useful for self-hosted Plausible).
    pub fn with_base_url(api_key: impl Into<String>, base_url: Url) -> Result<Self, ClientError> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(ClientError::Validation("api_key cannot be empty".into()));
        }
        let base_url = normalize_base_url(base_url);
        let user_agent = format!("plausible-cli/{}", env!("CARGO_PKG_VERSION"));
        let http = HttpClient::builder()
            .user_agent(&user_agent)
            .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .build()
            .map_err(ClientError::HttpClient)?;

        Ok(Self {
            http,
            base_url,
            api_key,
            user_agent,
        })
    }

    /// Returns the effective base URL.
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Returns the configured user agent string.
    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    fn endpoint(&self, fragment: &str) -> Result<Url, ClientError> {
        self.base_url
            .join(fragment)
            .map_err(ClientError::InvalidEndpoint)
    }

    /// Fetch the list of sites accessible to the API key.
    pub async fn list_sites(&self) -> Result<Vec<SiteSummary>, ClientError> {
        let url = self.endpoint("api/v1/sites")?;
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(ClientError::Http)?;
        self.handle_response(response).await
    }

    /// Create a Plausible site.
    pub async fn create_site(
        &self,
        request: &CreateSiteRequest,
    ) -> Result<SiteSummary, ClientError> {
        if request.domain.trim().is_empty() {
            return Err(ClientError::Validation("domain cannot be empty".into()));
        }
        let url = self.endpoint("api/v1/sites")?;
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(request)
            .send()
            .await
            .map_err(ClientError::Http)?;
        self.handle_response(response).await
    }

    /// Update mutable properties for a Plausible site.
    pub async fn update_site(
        &self,
        site_id: &str,
        request: &UpdateSiteRequest,
    ) -> Result<SiteSummary, ClientError> {
        if site_id.trim().is_empty() {
            return Err(ClientError::Validation("site_id cannot be empty".into()));
        }
        let url = self.endpoint(&format!("api/v1/sites/{site_id}"))?;
        let response = self
            .http
            .patch(url)
            .bearer_auth(&self.api_key)
            .json(request)
            .send()
            .await
            .map_err(ClientError::Http)?;
        self.handle_response(response).await
    }

    /// Reset statistics for a site.
    pub async fn reset_site_stats(
        &self,
        site_id: &str,
        request: &ResetSiteStatsRequest,
    ) -> Result<(), ClientError> {
        if site_id.trim().is_empty() {
            return Err(ClientError::Validation("site_id cannot be empty".into()));
        }
        let url = self.endpoint(&format!("api/v1/sites/{site_id}/reset-stats"))?;
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(request)
            .send()
            .await
            .map_err(ClientError::Http)?;
        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status();
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| String::from("unable to read error body"));
            Err(ClientError::Api { status, message })
        }
    }

    /// Delete a Plausible site.
    pub async fn delete_site(&self, site_id: &str) -> Result<(), ClientError> {
        if site_id.trim().is_empty() {
            return Err(ClientError::Validation("site_id cannot be empty".into()));
        }
        let url = self.endpoint(&format!("api/v1/sites/{site_id}"))?;
        let response = self
            .http
            .delete(url)
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(ClientError::Http)?;
        if response.status().is_success() || response.status() == StatusCode::NO_CONTENT {
            Ok(())
        } else {
            let status = response.status();
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| String::from("unable to read error body"));
            Err(ClientError::Api { status, message })
        }
    }

    /// Query aggregate statistics for a site.
    pub async fn stats_aggregate(
        &self,
        query: &AggregateQuery,
    ) -> Result<AggregateResponse, ClientError> {
        if query.site_id.trim().is_empty() {
            return Err(ClientError::Validation("site_id cannot be empty".into()));
        }
        if query.compare.is_some() {
            return Err(ClientError::Validation(
                "stats aggregate does not support --compare with the Stats API v2".into(),
            ));
        }
        if !query.properties.is_empty() {
            return Err(ClientError::Validation(
                "stats aggregate does not support --properties; use stats breakdown instead".into(),
            ));
        }
        let metrics = ensure_metrics(&query.metrics);
        let date_range = Some(resolve_date_range(
            query.period.as_ref(),
            query.date.as_ref(),
        )?);
        let filters = parse_filters(&query.filters)?;
        let order_by = parse_sort(query.sort.as_ref())?;
        let (limit, offset) = resolve_pagination(query.limit, query.page)?;
        if query.interval.is_some() {
            return Err(ClientError::Validation(
                "stats aggregate does not support --interval".into(),
            ));
        }
        let payload = StatsQueryPayload {
            site_id: query.site_id.clone(),
            metrics: metrics.clone(),
            date_range,
            dimensions: Vec::new(),
            filters,
            order_by,
            limit,
            offset,
            include: None,
        };
        let response = self.post_stats_query(&payload).await?;
        Ok(convert_aggregate_response(&metrics, response))
    }

    /// Query timeseries statistics for a site.
    pub async fn stats_timeseries(
        &self,
        query: &TimeseriesQuery,
    ) -> Result<TimeseriesResponse, ClientError> {
        if query.site_id.trim().is_empty() {
            return Err(ClientError::Validation("site_id cannot be empty".into()));
        }
        if query.compare.is_some() {
            return Err(ClientError::Validation(
                "stats timeseries does not support --compare with the Stats API v2".into(),
            ));
        }
        let metrics = ensure_metrics(&query.metrics);
        let date_range = Some(resolve_date_range(
            query.period.as_ref(),
            query.date.as_ref(),
        )?);
        let filters = parse_filters(&query.filters)?;
        let mut dimensions = Vec::new();
        dimensions.push(resolve_time_dimension(query.interval.as_deref())?);
        if !query.properties.is_empty() {
            dimensions.extend(query.properties.clone());
        }
        let order_by = parse_sort(query.sort.as_ref())?;
        let (limit, offset) = resolve_pagination(query.limit, query.page)?;
        let include = StatsInclude {
            time_labels: Some(true),
            ..StatsInclude::default()
        }
        .into_option();
        let payload = StatsQueryPayload {
            site_id: query.site_id.clone(),
            metrics: metrics.clone(),
            date_range,
            dimensions: dimensions.clone(),
            filters,
            order_by,
            limit,
            offset,
            include,
        };
        let response = self.post_stats_query(&payload).await?;
        Ok(convert_timeseries_response(&metrics, &dimensions, response))
    }

    /// Query breakdown statistics for a site.
    pub async fn stats_breakdown(
        &self,
        query: &BreakdownQuery,
    ) -> Result<BreakdownResponse, ClientError> {
        if query.site_id.trim().is_empty() {
            return Err(ClientError::Validation("site_id cannot be empty".into()));
        }
        if query.property.trim().is_empty() {
            return Err(ClientError::Validation("property cannot be empty".into()));
        }
        if query.compare.is_some() {
            return Err(ClientError::Validation(
                "stats breakdown does not support --compare with the Stats API v2".into(),
            ));
        }
        let metrics = ensure_metrics(&query.metrics);
        let date_range = Some(resolve_date_range(
            query.period.as_ref(),
            query.date.as_ref(),
        )?);
        let filters = parse_filters(&query.filters)?;
        let mut dimensions = vec![query.property.clone()];
        if !query.properties.is_empty() {
            dimensions.extend(query.properties.clone());
        }
        let order_by = parse_sort(query.sort.as_ref())?;
        let (limit, offset) = resolve_pagination(query.limit, query.page)?;
        let include = parse_include(query.include.as_ref(), limit.is_some())?;
        let payload = StatsQueryPayload {
            site_id: query.site_id.clone(),
            metrics: metrics.clone(),
            date_range,
            dimensions: dimensions.clone(),
            filters,
            order_by,
            limit,
            offset,
            include,
        };
        let response = self.post_stats_query(&payload).await?;
        Ok(convert_breakdown_response(
            &metrics,
            &dimensions,
            query.limit,
            query.page,
            response,
        ))
    }

    /// Fetch realtime visitor counts.
    pub async fn stats_realtime_visitors(
        &self,
        site_id: &str,
    ) -> Result<RealtimeVisitorsResponse, ClientError> {
        if site_id.trim().is_empty() {
            return Err(ClientError::Validation("site_id cannot be empty".into()));
        }
        let metrics = vec![
            "visitors".to_string(),
            "pageviews".to_string(),
            "bounce_rate".to_string(),
            "visit_duration".to_string(),
        ];
        let now = OffsetDateTime::now_utc();
        let window = TimeDuration::minutes(Self::REALTIME_WINDOW_MINUTES);
        let start = now.checked_sub(window).ok_or_else(|| {
            ClientError::Validation(
                "failed to compute realtime window; clock may be out of range".into(),
            )
        })?;
        let date_range = Some(StatsDateRange::Absolute {
            start: start.format(&Rfc3339).map_err(|err| {
                ClientError::Validation(format!("failed to format realtime start: {err}"))
            })?,
            end: now.format(&Rfc3339).map_err(|err| {
                ClientError::Validation(format!("failed to format realtime end: {err}"))
            })?,
        });
        let payload = StatsQueryPayload {
            site_id: site_id.to_string(),
            metrics: metrics.clone(),
            date_range,
            dimensions: Vec::new(),
            filters: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            include: None,
        };
        let response = self.post_stats_query(&payload).await?;
        Ok(convert_realtime_response(&metrics, response))
    }

    async fn post_stats_query(
        &self,
        payload: &StatsQueryPayload,
    ) -> Result<StatsQueryResponse, ClientError> {
        let url = self.endpoint("api/v2/query")?;
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(payload)
            .send()
            .await
            .map_err(ClientError::Http)?;
        self.handle_response(response).await
    }

    /// Send a custom event to Plausible.
    pub async fn send_event(&self, event: &Value) -> Result<(), ClientError> {
        if !event.is_object() {
            return Err(ClientError::Validation(
                "event payload must be a JSON object".into(),
            ));
        }
        let url = self.endpoint("api/v1/events")?;
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(event)
            .send()
            .await
            .map_err(ClientError::Http)?;
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| String::from("unable to read error body"));
            Err(ClientError::Api { status, message })
        }
    }

    async fn handle_response<T: DeserializeOwned>(
        &self,
        response: Response,
    ) -> Result<T, ClientError> {
        let status = response.status();
        if status.is_success() {
            response.json::<T>().await.map_err(ClientError::Http)
        } else {
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| String::from("unable to read error body"));
            Err(ClientError::Api { status, message })
        }
    }
}

fn normalize_base_url(mut url: Url) -> Url {
    if url.path().is_empty() {
        url.set_path("/");
    } else if !url.path().ends_with('/') {
        let mut path = url.path().trim_end_matches('/').to_string();
        path.push('/');
        url.set_path(&path);
    }
    url
}

/// Query parameters for the stats aggregate endpoint.
#[derive(Debug, Clone, Default)]
pub struct AggregateQuery {
    pub site_id: String,
    pub metrics: Vec<String>,
    pub period: Option<String>,
    pub date: Option<String>,
    pub filters: Vec<String>,
    pub properties: Vec<String>,
    pub compare: Option<String>,
    pub interval: Option<String>,
    pub sort: Option<String>,
    pub limit: Option<u32>,
    pub page: Option<u32>,
}

/// Response envelope for aggregate metrics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AggregateResponse {
    pub results: Map<String, Value>,
}

impl AggregateResponse {
    /// Convenience helper to access numeric metrics as f64 values.
    pub fn metric_as_f64(&self, metric: &str) -> Option<f64> {
        self.results.get(metric).and_then(|value| value.as_f64())
    }
}

/// Query parameters for the stats timeseries endpoint.
#[derive(Debug, Clone, Default)]
pub struct TimeseriesQuery {
    pub site_id: String,
    pub metrics: Vec<String>,
    pub period: Option<String>,
    pub date: Option<String>,
    pub interval: Option<String>,
    pub filters: Vec<String>,
    pub properties: Vec<String>,
    pub compare: Option<String>,
    pub sort: Option<String>,
    pub limit: Option<u32>,
    pub page: Option<u32>,
}

/// Response envelope for timeseries metrics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimeseriesResponse {
    #[serde(default)]
    pub results: Vec<Map<String, Value>>,
    #[serde(default)]
    pub totals: Map<String, Value>,
}

/// Query parameters for the stats breakdown endpoint.
#[derive(Debug, Clone, Default)]
pub struct BreakdownQuery {
    pub site_id: String,
    pub property: String,
    pub metrics: Vec<String>,
    pub period: Option<String>,
    pub date: Option<String>,
    pub filters: Vec<String>,
    pub properties: Vec<String>,
    pub compare: Option<String>,
    pub sort: Option<String>,
    pub limit: Option<u32>,
    pub page: Option<u32>,
    pub include: Option<String>,
}

/// Response envelope for breakdown metrics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BreakdownResponse {
    #[serde(default)]
    pub results: Vec<Map<String, Value>>,
    #[serde(default)]
    pub page: Option<u32>,
    #[serde(default)]
    pub total_pages: Option<u32>,
    #[serde(default)]
    pub totals: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Serialize)]
struct StatsQueryPayload {
    site_id: String,
    metrics: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    date_range: Option<StatsDateRange>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    dimensions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filters: Option<Vec<StatsFilterClause>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    order_by: Vec<StatsOrderClause>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    include: Option<StatsInclude>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum StatsDateRange {
    Preset(String),
    Absolute { start: String, end: String },
}

#[derive(Debug, Clone)]
struct StatsFilterClause {
    field: String,
    operation: String,
    values: Vec<String>,
}

impl serde::Serialize for StatsFilterClause {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(3))?;
        seq.serialize_element(&self.field)?;
        seq.serialize_element(&self.operation)?;
        if self.values.len() == 1 {
            seq.serialize_element(&self.values[0])?;
        } else {
            seq.serialize_element(&self.values)?;
        }
        seq.end()
    }
}

#[derive(Debug, Clone)]
struct StatsOrderClause {
    field: String,
    direction: String,
}

impl serde::Serialize for StatsOrderClause {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(2))?;
        seq.serialize_element(&self.field)?;
        seq.serialize_element(&self.direction)?;
        seq.end()
    }
}

#[derive(Debug, Clone, Default, Serialize)]
struct StatsInclude {
    #[serde(skip_serializing_if = "Option::is_none")]
    imports: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    time_labels: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_rows: Option<bool>,
}

impl StatsInclude {
    fn into_option(self) -> Option<Self> {
        let wants_imports = self.imports.unwrap_or(false);
        let wants_time = self.time_labels.unwrap_or(false);
        let wants_total = self.total_rows.unwrap_or(false);
        if wants_imports || wants_time || wants_total {
            Some(self)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
struct StatsQueryResponse {
    #[serde(default)]
    results: Vec<StatsQueryRow>,
    #[serde(default)]
    meta: StatsQueryMeta,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct StatsQueryRow {
    #[serde(default)]
    dimensions: Vec<Value>,
    #[serde(default)]
    metrics: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct StatsQueryMeta {
    #[serde(default)]
    total_rows: Option<u64>,
    #[serde(default)]
    time_labels: Option<Vec<Value>>,
    #[serde(default)]
    metric_totals: Option<Map<String, Value>>,
    #[serde(flatten)]
    _extra: Map<String, Value>,
}

fn ensure_metrics(metrics: &[String]) -> Vec<String> {
    if metrics.is_empty() {
        vec!["visitors".to_string()]
    } else {
        metrics.to_vec()
    }
}

fn resolve_date_range(
    period: Option<&String>,
    date: Option<&String>,
) -> Result<StatsDateRange, ClientError> {
    if let Some(date) = date {
        let mut parts = date.split(',').map(|p| p.trim()).filter(|p| !p.is_empty());
        let start = parts
            .next()
            .ok_or_else(|| ClientError::Validation("invalid --date range".into()))?;
        let end = parts
            .next()
            .ok_or_else(|| ClientError::Validation("invalid --date range".into()))?;
        if parts.next().is_some() {
            return Err(ClientError::Validation(
                "invalid --date range; expected start,end".into(),
            ));
        }
        return Ok(StatsDateRange::Absolute {
            start: start.to_string(),
            end: end.to_string(),
        });
    }
    let period = period
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .unwrap_or_else(|| "30d".to_string());
    Ok(StatsDateRange::Preset(period))
}

fn parse_filters(filters: &[String]) -> Result<Option<Vec<StatsFilterClause>>, ClientError> {
    if filters.is_empty() {
        return Ok(None);
    }
    let mut clauses = Vec::new();
    for entry in filters {
        for fragment in entry.split(';') {
            let trimmed = fragment.trim();
            if trimmed.is_empty() {
                continue;
            }
            clauses.push(parse_filter_clause(trimmed)?);
        }
    }
    if clauses.is_empty() {
        Ok(None)
    } else {
        Ok(Some(clauses))
    }
}

fn parse_filter_clause(raw: &str) -> Result<StatsFilterClause, ClientError> {
    const OPERATORS: [&str; 10] = ["!@", "=@", "!~", "=~", "!^", "=^", "!$", "$=", "!=", "=="];
    let mut chosen = None;
    for op in OPERATORS {
        if let Some(index) = raw.find(op) {
            chosen = Some((op, index));
            break;
        }
    }
    let (operator, position) = chosen.ok_or_else(|| {
        ClientError::Validation("unsupported filter expression for Stats API v2".into())
    })?;
    let field = raw[..position].trim();
    let value_part = raw[position + operator.len()..].trim();
    if field.is_empty() || value_part.is_empty() {
        return Err(ClientError::Validation(
            "filters must use <dimension><operator><value>".into(),
        ));
    }
    let (operation, values) = match operator {
        "==" => ("is".to_string(), split_filter_values(value_part)),
        "!=" => ("is_not".to_string(), split_filter_values(value_part)),
        "=@" => ("contains".to_string(), split_filter_values(value_part)),
        "!@" => ("contains_not".to_string(), split_filter_values(value_part)),
        "=~" => ("matches".to_string(), vec![value_part.to_string()]),
        "!~" => ("matches_not".to_string(), vec![value_part.to_string()]),
        "=^" => (
            "matches".to_string(),
            vec![format!("^{}", escape(value_part))],
        ),
        "!^" => (
            "matches_not".to_string(),
            vec![format!("^{}", escape(value_part))],
        ),
        "$=" => (
            "matches".to_string(),
            vec![format!("{}$", escape(value_part))],
        ),
        "!$" => (
            "matches_not".to_string(),
            vec![format!("{}$", escape(value_part))],
        ),
        _ => {
            return Err(ClientError::Validation(
                "unsupported filter operator for Stats API v2".into(),
            ))
        }
    };
    if values.is_empty() {
        return Err(ClientError::Validation(
            "filters require at least one value".into(),
        ));
    }
    Ok(StatsFilterClause {
        field: field.to_string(),
        operation,
        values,
    })
}

fn split_filter_values(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .collect()
}

fn parse_sort(sort: Option<&String>) -> Result<Vec<StatsOrderClause>, ClientError> {
    let Some(sort) = sort else {
        return Ok(Vec::new());
    };
    let mut clauses = Vec::new();
    for token in sort.split([',', ';']) {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.splitn(2, ':');
        let field = parts
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ClientError::Validation("invalid --sort value; expected field[:direction]".into())
            })?;
        let direction = parts.next().map(str::trim).unwrap_or("desc");
        let direction = match direction.to_ascii_lowercase().as_str() {
            "asc" => "asc",
            "desc" => "desc",
            other => {
                return Err(ClientError::Validation(format!(
                    "unsupported sort direction '{other}' for Stats API v2"
                )))
            }
        };
        clauses.push(StatsOrderClause {
            field: field.to_string(),
            direction: direction.to_string(),
        });
    }
    Ok(clauses)
}

fn resolve_pagination(
    limit: Option<u32>,
    page: Option<u32>,
) -> Result<(Option<u32>, Option<u32>), ClientError> {
    match (limit, page) {
        (Some(limit), Some(page)) => {
            if page == 0 {
                return Err(ClientError::Validation(
                    "page must be at least 1 when using the Stats API v2".into(),
                ));
            }
            let offset = (page as u64 - 1) * limit as u64;
            if offset > u32::MAX as u64 {
                return Err(ClientError::Validation(
                    "requested page is too large for Stats API pagination".into(),
                ));
            }
            Ok((Some(limit), Some(offset as u32)))
        }
        (Some(limit), None) => Ok((Some(limit), None)),
        (None, Some(_)) => Err(ClientError::Validation(
            "stats queries require --limit when --page is provided".into(),
        )),
        (None, None) => Ok((None, None)),
    }
}

fn resolve_time_dimension(interval: Option<&str>) -> Result<String, ClientError> {
    match interval.map(|s| s.to_ascii_lowercase()) {
        None => Ok("time:day".into()),
        Some(value) => match value.as_str() {
            "date" | "day" => Ok("time:day".into()),
            "hour" | "hours" => Ok("time:hour".into()),
            "week" | "weekly" => Ok("time:week".into()),
            "month" | "monthly" => Ok("time:month".into()),
            "year" | "yearly" => Ok("time:year".into()),
            "minute" | "minutes" => Ok("time:minute".into()),
            other => Err(ClientError::Validation(format!(
                "unsupported --interval '{other}' for Stats API v2"
            ))),
        },
    }
}

fn canonical_dimension_key(dimension: &str, index: usize) -> String {
    if dimension == "time" || dimension.starts_with("time:") {
        "time".into()
    } else if let Some((_, suffix)) = dimension.rsplit_once(':') {
        suffix.to_string()
    } else {
        format!("dimension_{index}")
    }
}

fn convert_aggregate_response(
    metrics: &[String],
    response: StatsQueryResponse,
) -> AggregateResponse {
    let mut results = Map::new();
    if let Some(row) = response.results.first() {
        for (index, metric) in metrics.iter().enumerate() {
            let value = row.metrics.get(index).cloned().unwrap_or(Value::Null);
            results.insert(metric.clone(), value);
        }
    } else {
        for metric in metrics {
            results.insert(metric.clone(), Value::Number(serde_json::Number::from(0)));
        }
    }
    AggregateResponse { results }
}

fn convert_timeseries_response(
    metrics: &[String],
    dimensions: &[String],
    mut response: StatsQueryResponse,
) -> TimeseriesResponse {
    if let Some(labels) = response.meta.time_labels.take() {
        if !labels.is_empty() {
            response.results =
                fill_time_labels(dimensions, metrics.len(), response.results, labels);
        }
    }
    let mut rows = Vec::with_capacity(response.results.len());
    for entry in response.results {
        let mut map = Map::new();
        for (index, value) in entry.dimensions.into_iter().enumerate() {
            let key = if index == 0 {
                "time".to_string()
            } else {
                canonical_dimension_key(
                    dimensions
                        .get(index)
                        .map(String::as_str)
                        .unwrap_or("dimension"),
                    index,
                )
            };
            map.insert(key, value);
        }
        for (index, metric) in metrics.iter().enumerate() {
            let value = entry.metrics.get(index).cloned().unwrap_or(Value::Null);
            map.insert(metric.clone(), value);
        }
        rows.push(map);
    }
    let totals = response.meta.metric_totals.unwrap_or_default();
    TimeseriesResponse {
        results: rows,
        totals,
    }
}

fn fill_time_labels(
    dimensions: &[String],
    metrics_len: usize,
    existing: Vec<StatsQueryRow>,
    labels: Vec<Value>,
) -> Vec<StatsQueryRow> {
    if dimensions.is_empty() {
        return existing;
    }
    let mut filled = Vec::with_capacity(labels.len());
    let mut iter = existing.into_iter().peekable();
    for label in labels {
        if let Some(next) = iter.peek() {
            if next.dimensions.first() == Some(&label) {
                filled.push(iter.next().unwrap());
                continue;
            }
        }
        let metrics = if metrics_len > 0 {
            (0..metrics_len)
                .map(|_| Value::Number(Number::from(0)))
                .collect()
        } else {
            Vec::new()
        };
        let row = StatsQueryRow {
            dimensions: vec![label],
            metrics,
        };
        filled.push(row);
    }
    filled
}

fn convert_breakdown_response(
    metrics: &[String],
    dimensions: &[String],
    limit: Option<u32>,
    page: Option<u32>,
    response: StatsQueryResponse,
) -> BreakdownResponse {
    let mut rows = Vec::with_capacity(response.results.len());
    for entry in response.results {
        let mut map = Map::new();
        for (index, value) in entry.dimensions.into_iter().enumerate() {
            if index == 0 {
                map.insert("value".into(), value);
            } else {
                let key = dimensions
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| format!("dimension_{index}"));
                map.insert(key, value);
            }
        }
        for (index, metric) in metrics.iter().enumerate() {
            let value = entry.metrics.get(index).cloned().unwrap_or(Value::Null);
            map.insert(metric.clone(), value);
        }
        rows.push(map);
    }
    let totals = response.meta.metric_totals.filter(|map| !map.is_empty());
    let total_pages = response
        .meta
        .total_rows
        .and_then(|rows| limit.map(|limit| rows.div_ceil(limit as u64) as u32));
    BreakdownResponse {
        results: rows,
        page,
        total_pages,
        totals,
    }
}

fn convert_realtime_response(
    metrics: &[String],
    response: StatsQueryResponse,
) -> RealtimeVisitorsResponse {
    let mut realtime = RealtimeVisitorsResponse::default();
    if let Some(row) = response.results.first() {
        for (index, metric) in metrics.iter().enumerate() {
            let value = row.metrics.get(index).unwrap_or(&Value::Null);
            match metric.as_str() {
                "visitors" => {
                    realtime.visitors = value.as_i64().unwrap_or_else(|| {
                        value
                            .as_f64()
                            .map(|v| v as i64)
                            .unwrap_or_else(|| realtime.visitors)
                    });
                }
                "pageviews" => {
                    realtime.pageviews =
                        value.as_i64().or_else(|| value.as_f64().map(|v| v as i64));
                }
                "bounce_rate" => {
                    if let Some(rate) = value.as_f64() {
                        realtime.bounce_rate = Some(if rate > 1.0 { rate / 100.0 } else { rate });
                    }
                }
                "visit_duration" => {
                    if let Some(duration) = value.as_f64() {
                        realtime.visit_duration = Some(duration);
                    } else if let Some(duration) = value.as_i64() {
                        realtime.visit_duration = Some(duration as f64);
                    }
                }
                _ => {}
            }
        }
    }
    realtime
}

fn parse_include(
    include: Option<&String>,
    needs_total_rows: bool,
) -> Result<Option<StatsInclude>, ClientError> {
    let mut options = StatsInclude::default();
    if let Some(include) = include {
        for token in include.split([',', ';']) {
            let trimmed = token.trim().to_ascii_lowercase();
            if trimmed.is_empty() {
                continue;
            }
            match trimmed.as_str() {
                "imports" | "imported" => options.imports = Some(true),
                "time_labels" | "time-labels" | "labels" => options.time_labels = Some(true),
                "total_rows" | "total-rows" | "totals" => options.total_rows = Some(true),
                other => {
                    return Err(ClientError::Validation(format!(
                        "unsupported --include option '{other}' for Stats API v2"
                    )));
                }
            }
        }
    }
    if needs_total_rows {
        options.total_rows = Some(true);
    }
    Ok(options.into_option())
}

/// Minimal representation of a Plausible site entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SiteSummary {
    pub domain: String,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub is_main_site: Option<bool>,
    #[serde(default)]
    pub public: Option<bool>,
    #[serde(default)]
    pub verified: Option<bool>,
}

/// Request payload to create a new site.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateSiteRequest {
    pub domain: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public: Option<bool>,
}

/// Request payload to update an existing site.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct UpdateSiteRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public: Option<bool>,
    #[serde(rename = "is_main_site", skip_serializing_if = "Option::is_none")]
    pub main_site: Option<bool>,
}

/// Request payload for resetting site statistics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ResetSiteStatsRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
}

/// Response payload for realtime visitors.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RealtimeVisitorsResponse {
    pub visitors: i64,
    #[serde(default)]
    pub pageviews: Option<i64>,
    #[serde(default)]
    pub bounce_rate: Option<f64>,
    #[serde(default)]
    pub visit_duration: Option<f64>,
}

#[derive(thiserror::Error, Debug)]
pub enum ClientError {
    #[error("invalid base URL: {0}")]
    InvalidBaseUrl(#[source] url::ParseError),
    #[error("invalid endpoint: {0}")]
    InvalidEndpoint(#[source] url::ParseError),
    #[error("HTTP client build error: {0}")]
    HttpClient(#[source] reqwest::Error),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("request validation failed: {0}")]
    Validation(String),
    #[error("API request failed with status {status}: {message}")]
    Api { status: StatusCode, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::{prelude::*, Method::PATCH};
    use serde_json::json;

    fn test_client(api_key: &str, server: &MockServer) -> PlausibleClient {
        let url = Url::parse(&format!("{}/", server.base_url())).expect("url parse");
        PlausibleClient::with_base_url(api_key, url).expect("client")
    }

    #[tokio::test]
    async fn list_sites_fetches_with_auth_header() {
        let server = MockServer::start_async().await;

        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/api/v1/sites")
                    .header("authorization", "Bearer secret");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(json!([
                        {
                            "domain": "example.com",
                            "timezone": "UTC",
                            "is_main_site": false,
                            "public": false,
                            "verified": true
                        }
                    ]));
            })
            .await;

        let client = test_client("secret", &server);
        let sites = client.list_sites().await.expect("list sites");
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].domain, "example.com");
        assert_eq!(sites[0].timezone.as_deref(), Some("UTC"));
    }

    #[tokio::test]
    async fn stats_aggregate_builds_expected_query() {
        let server = MockServer::start_async().await;

        let expected_body = json!({
            "site_id": "example.com",
            "metrics": ["visitors", "pageviews"],
            "date_range": "30d",
            "filters": [
                ["event:page", "is", "/docs"]
            ]
        });

        server
            .mock_async(move |when, then| {
                when.method(POST)
                    .path("/api/v2/query")
                    .header("authorization", "Bearer test-key")
                    .json_body(expected_body.clone());
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(json!({
                        "results": [
                            {
                                "dimensions": [],
                                "metrics": [120, 350]
                            }
                        ],
                        "meta": {
                            "metric_totals": {
                                "visitors": 120,
                                "pageviews": 350
                            }
                        }
                    }));
            })
            .await;

        let client = test_client("test-key", &server);
        let query = AggregateQuery {
            site_id: "example.com".into(),
            metrics: vec!["visitors".into(), "pageviews".into()],
            filters: vec!["event:page==/docs".into()],
            ..AggregateQuery::default()
        };

        let response = client.stats_aggregate(&query).await.expect("aggregate");
        assert_eq!(response.metric_as_f64("visitors"), Some(120.0));
        assert_eq!(response.metric_as_f64("pageviews"), Some(350.0));
    }

    #[tokio::test]
    async fn stats_aggregate_requires_site_id() {
        let server = MockServer::start_async().await;
        let client = test_client("secret", &server);
        let query = AggregateQuery::default();
        let err = client
            .stats_aggregate(&query)
            .await
            .expect_err("validation");
        assert!(matches!(err, ClientError::Validation(msg) if msg.contains("site_id")));
    }

    #[tokio::test]
    async fn non_success_status_returns_api_error() {
        let server = MockServer::start_async().await;

        server
            .mock_async(|when, then| {
                when.method(GET).path("/api/v1/sites");
                then.status(401)
                    .header("content-type", "text/plain")
                    .body("unauthorized");
            })
            .await;

        let client = test_client("secret", &server);
        let err = client.list_sites().await.expect_err("api error");
        assert!(
            matches!(err, ClientError::Api { status, .. } if status == StatusCode::UNAUTHORIZED)
        );
    }

    #[tokio::test]
    async fn stats_timeseries_hits_endpoint_with_query() {
        let server = MockServer::start_async().await;

        let expected_body = json!({
            "site_id": "example.com",
            "metrics": ["visitors"],
            "date_range": "30d",
            "dimensions": ["time:day"],
            "include": { "time_labels": true }
        });

        server
            .mock_async(move |when, then| {
                when.method(POST)
                    .path("/api/v2/query")
                    .header("authorization", "Bearer key-123")
                    .json_body(expected_body.clone());
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(json!({
                        "results": [
                            { "dimensions": ["2024-01-01"], "metrics": [10] },
                            { "dimensions": ["2024-01-02"], "metrics": [12] }
                        ],
                        "meta": {
                            "metric_totals": { "visitors": 22 }
                        }
                    }));
            })
            .await;

        let client = test_client("key-123", &server);
        let query = TimeseriesQuery {
            site_id: "example.com".into(),
            metrics: vec!["visitors".into()],
            interval: Some("date".into()),
            ..TimeseriesQuery::default()
        };

        let response = client
            .stats_timeseries(&query)
            .await
            .expect("timeseries response");
        assert_eq!(response.results.len(), 2);
        assert_eq!(response.results[0].get("time"), Some(&json!("2024-01-01")));
        assert_eq!(response.results[0].get("visitors"), Some(&json!(10)));
        assert_eq!(
            response.totals.get("visitors").and_then(|v| v.as_i64()),
            Some(22)
        );
    }

    #[tokio::test]
    async fn stats_breakdown_hits_endpoint_with_query() {
        let server = MockServer::start_async().await;

        let expected_body = json!({
            "site_id": "example.com",
            "metrics": ["visitors"],
            "date_range": "30d",
            "dimensions": ["event:page"],
            "limit": 50,
            "offset": 0,
            "include": { "total_rows": true }
        });

        server
            .mock_async(move |when, then| {
                when.method(POST)
                    .path("/api/v2/query")
                    .header("authorization", "Bearer breakdown-key")
                    .json_body(expected_body.clone());
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(json!({
                        "results": [
                            { "dimensions": ["/docs"], "metrics": [50] },
                            { "dimensions": ["/blog"], "metrics": [30] }
                        ],
                        "meta": {
                            "total_rows": 2,
                            "metric_totals": { "visitors": 80 }
                        }
                    }));
            })
            .await;

        let client = test_client("breakdown-key", &server);
        let query = BreakdownQuery {
            site_id: "example.com".into(),
            property: "event:page".into(),
            metrics: vec!["visitors".into()],
            limit: Some(50),
            page: Some(1),
            ..BreakdownQuery::default()
        };

        let response = client
            .stats_breakdown(&query)
            .await
            .expect("breakdown response");
        assert_eq!(response.results.len(), 2);
        assert_eq!(response.results[0].get("value"), Some(&json!("/docs")));
        assert_eq!(response.page, Some(1));
        assert_eq!(response.total_pages, Some(1));
    }

    #[tokio::test]
    async fn send_event_posts_payload() {
        let server = MockServer::start_async().await;

        server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/api/v1/events")
                    .header("authorization", "Bearer event-key")
                    .json_body(json!({
                        "name": "Signup",
                        "domain": "example.com"
                    }));
                then.status(202);
            })
            .await;

        let client = test_client("event-key", &server);
        let event = json!({
            "name": "Signup",
            "domain": "example.com"
        });
        client.send_event(&event).await.expect("send event");
    }

    #[tokio::test]
    async fn send_event_rejects_non_object_payload() {
        let server = MockServer::start_async().await;
        let client = test_client("key", &server);
        let event = serde_json::Value::String("not-object".into());
        let err = client.send_event(&event).await.expect_err("validation");
        assert!(matches!(err, ClientError::Validation(msg) if msg.contains("JSON object")));
    }

    #[tokio::test]
    async fn create_site_posts_payload() {
        let server = MockServer::start_async().await;

        server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/api/v1/sites")
                    .header("authorization", "Bearer site-key")
                    .json_body(json!({
                        "domain": "example.com",
                        "timezone": "UTC",
                        "public": true
                    }));
                then.status(201)
                    .header("content-type", "application/json")
                    .json_body(json!({
                        "domain": "example.com",
                        "timezone": "UTC",
                        "public": true,
                        "verified": false
                    }));
            })
            .await;

        let client = test_client("site-key", &server);
        let site = client
            .create_site(&CreateSiteRequest {
                domain: "example.com".into(),
                timezone: Some("UTC".into()),
                public: Some(true),
            })
            .await
            .expect("create site");
        assert_eq!(site.domain, "example.com");
        assert_eq!(site.public, Some(true));
    }

    #[tokio::test]
    async fn update_site_sends_patch_body() {
        let server = MockServer::start_async().await;

        server
            .mock_async(|when, then| {
                when.method(PATCH)
                    .path("/api/v1/sites/example.com")
                    .header("authorization", "Bearer update-key")
                    .json_body(json!({ "timezone": "Europe/Berlin" }));
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(json!({
                        "domain": "example.com",
                        "timezone": "Europe/Berlin"
                    }));
            })
            .await;

        let client = test_client("update-key", &server);
        let site = client
            .update_site(
                "example.com",
                &UpdateSiteRequest {
                    timezone: Some("Europe/Berlin".into()),
                    public: None,
                    main_site: None,
                },
            )
            .await
            .expect("update site");
        assert_eq!(site.timezone.as_deref(), Some("Europe/Berlin"));
    }

    #[tokio::test]
    async fn reset_site_stats_posts_date_range() {
        let server = MockServer::start_async().await;

        server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/api/v1/sites/example.com/reset-stats")
                    .header("authorization", "Bearer reset-key")
                    .json_body(json!({ "date": "2024-01-01" }));
                then.status(202);
            })
            .await;

        let client = test_client("reset-key", &server);
        client
            .reset_site_stats(
                "example.com",
                &ResetSiteStatsRequest {
                    date: Some("2024-01-01".into()),
                },
            )
            .await
            .expect("reset stats");
    }

    #[tokio::test]
    async fn delete_site_issues_delete() {
        let server = MockServer::start_async().await;

        server
            .mock_async(|when, then| {
                when.method(DELETE)
                    .path("/api/v1/sites/example.com")
                    .header("authorization", "Bearer delete-key");
                then.status(204);
            })
            .await;

        let client = test_client("delete-key", &server);
        client
            .delete_site("example.com")
            .await
            .expect("delete site");
    }

    #[tokio::test]
    async fn realtime_visitors_fetches_metrics() {
        let server = MockServer::start_async().await;

        server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/api/v2/query")
                    .header("authorization", "Bearer realtime-key")
                    .body_contains("\"site_id\":\"example.com\"")
                    .body_contains("\"metrics\":[\"visitors\",\"pageviews\",\"bounce_rate\",\"visit_duration\"]");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(json!({
                        "results": [
                            {
                                "dimensions": [],
                                "metrics": [5, 7, 45.0, 120.0]
                            }
                        ]
                    }));
            })
            .await;

        let client = test_client("realtime-key", &server);
        let realtime = client
            .stats_realtime_visitors("example.com")
            .await
            .expect("realtime stats");
        assert_eq!(realtime.visitors, 5);
        assert_eq!(realtime.pageviews, Some(7));
        assert_eq!(realtime.bounce_rate, Some(0.45));
        assert_eq!(realtime.visit_duration, Some(120.0));
    }
}
