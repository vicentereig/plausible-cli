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

    /// Query aggregate statistics for a site.
    pub async fn stats_aggregate(
        &self,
        query: &AggregateQuery,
    ) -> Result<AggregateResponse, ClientError> {
        if query.site_id.trim().is_empty() {
            return Err(ClientError::Validation("site_id cannot be empty"));
        }
        let url = self.endpoint("api/v1/stats/aggregate")?;
        let mut params: Vec<(String, String)> = Vec::new();
        params.push(("site_id".into(), query.site_id.clone()));

        let metrics = if query.metrics.is_empty() {
            vec!["visitors".to_string()]
        } else {
            query.metrics.clone()
        };
        params.push(("metrics".into(), metrics.join(",")));

        if let Some(period) = query.period.as_ref() {
            params.push(("period".into(), period.clone()));
        }
        if let Some(date) = query.date.as_ref() {
            params.push(("date".into(), date.clone()));
        }
        if !query.filters.is_empty() {
            params.push(("filters".into(), query.filters.join(";")));
        }
        if !query.properties.is_empty() {
            params.push(("properties".into(), query.properties.join(";")));
        }
        if let Some(compare) = query.compare.as_ref() {
            params.push(("compare".into(), compare.clone()));
        }
        if let Some(interval) = query.interval.as_ref() {
            params.push(("interval".into(), interval.clone()));
        }
        if let Some(sort) = query.sort.as_ref() {
            params.push(("sort".into(), sort.clone()));
        }
        if let Some(limit) = query.limit {
            params.push(("limit".into(), limit.to_string()));
        }
        if let Some(page) = query.page {
            params.push(("page".into(), page.to_string()));
        }

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
    use httpmock::prelude::*;
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
}
