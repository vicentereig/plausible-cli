use reqwest::{Client as HttpClient, Response, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{Map, Value};
use std::time::Duration;
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
    /// Create a new client targeting the public Plausible API endpoint.
    pub fn new(api_key: impl Into<String>) -> Result<Self, ClientError> {
        let base = Url::parse(DEFAULT_BASE_URL).map_err(ClientError::InvalidBaseUrl)?;
        Self::with_base_url(api_key, base)
    }

    /// Create a client with a custom base URL (useful for self-hosted Plausible).
    pub fn with_base_url(api_key: impl Into<String>, base_url: Url) -> Result<Self, ClientError> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(ClientError::Validation("api_key cannot be empty"));
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
            return Err(ClientError::Validation("domain cannot be empty"));
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
            return Err(ClientError::Validation("site_id cannot be empty"));
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
            return Err(ClientError::Validation("site_id cannot be empty"));
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
            return Err(ClientError::Validation("site_id cannot be empty"));
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
            return Err(ClientError::Validation("site_id cannot be empty"));
        }
        let url = self.endpoint("api/v1/stats/aggregate")?;
        let params = build_stats_query(
            &query.site_id,
            Some(&query.metrics),
            &StatsQueryOptions {
                period: query.period.as_ref(),
                date: query.date.as_ref(),
                filters: &query.filters,
                properties: &query.properties,
                compare: query.compare.as_ref(),
                interval: query.interval.as_ref(),
                sort: query.sort.as_ref(),
                limit: query.limit,
                page: query.page,
                include: None,
                property: None,
            },
        );

        let response = self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
            .query(&params)
            .send()
            .await
            .map_err(ClientError::Http)?;
        self.handle_response(response).await
    }

    /// Query timeseries statistics for a site.
    pub async fn stats_timeseries(
        &self,
        query: &TimeseriesQuery,
    ) -> Result<TimeseriesResponse, ClientError> {
        if query.site_id.trim().is_empty() {
            return Err(ClientError::Validation("site_id cannot be empty"));
        }
        let url = self.endpoint("api/v1/stats/timeseries")?;
        let params = build_stats_query(
            &query.site_id,
            Some(&query.metrics),
            &StatsQueryOptions {
                period: query.period.as_ref(),
                date: query.date.as_ref(),
                filters: &query.filters,
                properties: &query.properties,
                compare: query.compare.as_ref(),
                interval: query.interval.as_ref(),
                sort: query.sort.as_ref(),
                limit: query.limit,
                page: query.page,
                include: None,
                property: None,
            },
        );
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
            .query(&params)
            .send()
            .await
            .map_err(ClientError::Http)?;
        self.handle_response(response).await
    }

    /// Query breakdown statistics for a site.
    pub async fn stats_breakdown(
        &self,
        query: &BreakdownQuery,
    ) -> Result<BreakdownResponse, ClientError> {
        if query.site_id.trim().is_empty() {
            return Err(ClientError::Validation("site_id cannot be empty"));
        }
        if query.property.trim().is_empty() {
            return Err(ClientError::Validation("property cannot be empty"));
        }
        let url = self.endpoint("api/v1/stats/breakdown")?;
        let params = build_stats_query(
            &query.site_id,
            Some(&query.metrics),
            &StatsQueryOptions {
                period: query.period.as_ref(),
                date: query.date.as_ref(),
                filters: &query.filters,
                properties: &query.properties,
                compare: query.compare.as_ref(),
                interval: None,
                sort: query.sort.as_ref(),
                limit: query.limit,
                page: query.page,
                include: query.include.as_ref(),
                property: Some(&query.property),
            },
        );
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
            .query(&params)
            .send()
            .await
            .map_err(ClientError::Http)?;
        self.handle_response(response).await
    }

    /// Fetch realtime visitor counts.
    pub async fn stats_realtime_visitors(
        &self,
        site_id: &str,
    ) -> Result<RealtimeVisitorsResponse, ClientError> {
        if site_id.trim().is_empty() {
            return Err(ClientError::Validation("site_id cannot be empty"));
        }
        let url = self.endpoint("api/v1/stats/realtime/visitors")?;
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
            .query(&[("site_id", site_id)])
            .send()
            .await
            .map_err(ClientError::Http)?;
        self.handle_response(response).await
    }

    /// Send a custom event to Plausible.
    pub async fn send_event(&self, event: &Value) -> Result<(), ClientError> {
        if !event.is_object() {
            return Err(ClientError::Validation(
                "event payload must be a JSON object",
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

struct StatsQueryOptions<'a> {
    period: Option<&'a String>,
    date: Option<&'a String>,
    filters: &'a [String],
    properties: &'a [String],
    compare: Option<&'a String>,
    interval: Option<&'a String>,
    sort: Option<&'a String>,
    limit: Option<u32>,
    page: Option<u32>,
    include: Option<&'a String>,
    property: Option<&'a String>,
}

fn build_stats_query(
    site_id: &str,
    metrics: Option<&[String]>,
    options: &StatsQueryOptions<'_>,
) -> Vec<(String, String)> {
    let mut params: Vec<(String, String)> = Vec::new();
    params.push(("site_id".into(), site_id.to_string()));
    let metrics = if let Some(metrics) = metrics {
        if metrics.is_empty() {
            vec!["visitors".to_string()]
        } else {
            metrics.to_vec()
        }
    } else {
        vec!["visitors".to_string()]
    };
    params.push(("metrics".into(), metrics.join(",")));

    if let Some(period) = options.period {
        params.push(("period".into(), period.clone()));
    }
    if let Some(date) = options.date {
        params.push(("date".into(), date.clone()));
    }
    if !options.filters.is_empty() {
        params.push(("filters".into(), options.filters.join(";")));
    }
    if !options.properties.is_empty() {
        params.push(("properties".into(), options.properties.join(";")));
    }
    if let Some(compare) = options.compare {
        params.push(("compare".into(), compare.clone()));
    }
    if let Some(interval) = options.interval {
        params.push(("interval".into(), interval.clone()));
    }
    if let Some(sort) = options.sort {
        params.push(("sort".into(), sort.clone()));
    }
    if let Some(limit) = options.limit {
        params.push(("limit".into(), limit.to_string()));
    }
    if let Some(page) = options.page {
        params.push(("page".into(), page.to_string()));
    }
    if let Some(include) = options.include {
        params.push(("include".into(), include.clone()));
    }
    if let Some(property) = options.property {
        params.push(("property".into(), property.clone()));
    }
    params
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
    Validation(&'static str),
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

        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/api/v1/stats/aggregate")
                    .header("authorization", "Bearer test-key")
                    .query_param("site_id", "example.com")
                    .query_param("metrics", "visitors,pageviews")
                    .query_param("filters", "event:page==/docs");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(json!({
                        "results": {
                            "visitors": 120,
                            "pageviews": 350
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

        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/api/v1/stats/timeseries")
                    .header("authorization", "Bearer key-123")
                    .query_param("site_id", "example.com")
                    .query_param("metrics", "visitors")
                    .query_param("interval", "date");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(json!({
                        "results": [
                            { "date": "2024-01-01", "visitors": 10 },
                            { "date": "2024-01-02", "visitors": 12 }
                        ],
                        "totals": { "visitors": 22 }
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
        assert_eq!(response.results[0].get("visitors"), Some(&json!(10)));
        assert_eq!(
            response.totals.get("visitors").and_then(|v| v.as_i64()),
            Some(22)
        );
    }

    #[tokio::test]
    async fn stats_breakdown_hits_endpoint_with_query() {
        let server = MockServer::start_async().await;

        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/api/v1/stats/breakdown")
                    .header("authorization", "Bearer breakdown-key")
                    .query_param("site_id", "example.com")
                    .query_param("property", "event:page")
                    .query_param("metrics", "visitors");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(json!({
                        "results": [
                            { "value": "/docs", "visitors": 50 },
                            { "value": "/blog", "visitors": 30 }
                        ],
                        "page": 1,
                        "total_pages": 1
                    }));
            })
            .await;

        let client = test_client("breakdown-key", &server);
        let query = BreakdownQuery {
            site_id: "example.com".into(),
            property: "event:page".into(),
            metrics: vec!["visitors".into()],
            ..BreakdownQuery::default()
        };

        let response = client
            .stats_breakdown(&query)
            .await
            .expect("breakdown response");
        assert_eq!(response.results.len(), 2);
        assert_eq!(response.results[0].get("value"), Some(&json!("/docs")));
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
                when.method(GET)
                    .path("/api/v1/stats/realtime/visitors")
                    .header("authorization", "Bearer realtime-key")
                    .query_param("site_id", "example.com");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(json!({
                        "visitors": 5,
                        "pageviews": 7
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
    }
}
