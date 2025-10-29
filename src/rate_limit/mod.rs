use governor::{
    clock::DefaultClock,
    state::{direct::NotKeyed, InMemoryState},
    Jitter, Quota, RateLimiter as GovernorRateLimiter,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fs;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration as StdDuration;
use time::{
    format_description::FormatItem, macros::format_description, Date, Duration as TimeDuration,
    OffsetDateTime,
};
use tokio::sync::Mutex;

use crate::config::ConfigPaths;

const DEFAULT_JITTER_UP_TO_MS: u64 = 250;

/// Rate limiting configuration combining hourly quota, optional daily ceiling, and jitter.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    pub hourly_quota: NonZeroU32,
    pub daily_quota: Option<NonZeroU32>,
    pub jitter_max: StdDuration,
    quota: Quota,
}

impl RateLimitConfig {
    pub fn new(hourly_quota: NonZeroU32) -> Self {
        let quota = Quota::per_hour(hourly_quota);
        Self {
            hourly_quota,
            daily_quota: Some(hourly_quota),
            jitter_max: StdDuration::from_millis(DEFAULT_JITTER_UP_TO_MS),
            quota,
        }
    }

    pub fn with_daily_quota(mut self, daily: Option<NonZeroU32>) -> Self {
        self.daily_quota = daily;
        self
    }

    #[cfg(test)]
    pub fn with_quota(mut self, quota: Quota) -> Self {
        self.quota = quota;
        self
    }

    fn jitter(&self) -> Jitter {
        if self.jitter_max.is_zero() {
            Jitter::up_to(StdDuration::from_millis(0))
        } else {
            Jitter::up_to(self.jitter_max)
        }
    }

    fn quota(&self) -> Quota {
        self.quota
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        RateLimitConfig::new(NonZeroU32::new(600).expect("non zero"))
    }
}

/// Aggregate rate limiter coordinating in-memory throttling with persisted usage counters.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    limiter: Arc<GovernorRateLimiter<NotKeyed, InMemoryState, DefaultClock>>,
    usage: Arc<Mutex<UsageLedger>>,
    config: RateLimitConfig,
    jitter: Jitter,
}

impl RateLimiter {
    pub async fn new(
        paths: ConfigPaths,
        account: &str,
        config: RateLimitConfig,
    ) -> Result<Self, RateLimitError> {
        paths.ensure_exists()?;
        let usage = UsageLedger::load(paths, account.to_string())?;
        let limiter = GovernorRateLimiter::direct(config.quota());
        Ok(Self {
            limiter: Arc::new(limiter),
            usage: Arc::new(Mutex::new(usage)),
            jitter: config.jitter(),
            config,
        })
    }

    /// Wait until the configured quota permits executing `weight` cost units.
    pub async fn acquire(&self, weight: NonZeroU32) {
        let _ = self
            .limiter
            .until_n_ready_with_jitter(weight, self.jitter)
            .await;
    }

    /// Record a successful request, updating persisted counters.
    pub async fn record_success(
        &self,
        weight: u32,
        now: OffsetDateTime,
    ) -> Result<RateStatus, RateLimitError> {
        let mut usage = self.usage.lock().await;
        usage.record(weight, now)?;
        usage.persist()?;
        Ok(usage.status(&self.config, now))
    }

    /// Retrieve the current counter status without mutating state.
    pub async fn status(&self, now: OffsetDateTime) -> Result<RateStatus, RateLimitError> {
        let usage = self.usage.lock().await;
        Ok(usage.status(&self.config, now))
    }
}

#[derive(Debug, Clone)]
struct UsageLedger {
    path: PathBuf,
    snapshot: UsageSnapshot,
}

impl UsageLedger {
    fn load(paths: ConfigPaths, account: String) -> Result<Self, RateLimitError> {
        let path = paths.usage_dir().join(format!("{account}.json"));
        let snapshot = match fs::read(&path) {
            Ok(bytes) => {
                if bytes.is_empty() {
                    UsageSnapshot::new(account.clone())
                } else {
                    serde_json::from_slice(&bytes).map_err(RateLimitError::Deserialize)?
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                UsageSnapshot::new(account.clone())
            }
            Err(source) => return Err(RateLimitError::Io { path, source }),
        };
        Ok(Self { path, snapshot })
    }

    fn persist(&self) -> Result<(), RateLimitError> {
        let contents =
            serde_json::to_vec_pretty(&self.snapshot).map_err(RateLimitError::Serialize)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| RateLimitError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::write(&self.path, contents).map_err(|source| RateLimitError::Io {
            path: self.path.clone(),
            source,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&self.path)
                .map_err(|source| RateLimitError::Io {
                    path: self.path.clone(),
                    source,
                })?
                .permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&self.path, perms).map_err(|source| RateLimitError::Io {
                path: self.path.clone(),
                source,
            })?;
        }
        Ok(())
    }

    fn record(&mut self, amount: u32, now: OffsetDateTime) -> Result<(), RateLimitError> {
        let hour = hour_bucket(now);
        let entry = self.snapshot.hourly.get_or_insert(HourWindow {
            start: hour,
            count: 0,
        });
        if entry.start != hour {
            entry.start = hour;
            entry.count = 0;
        }
        entry.count = entry.count.saturating_add(amount);

        let day = now.date();
        let daily = self.snapshot.daily.get_or_insert(DayWindow {
            date: day,
            count: 0,
        });
        if daily.date != day {
            daily.date = day;
            daily.count = 0;
        }
        daily.count = daily.count.saturating_add(amount);
        Ok(())
    }

    fn status(&self, config: &RateLimitConfig, now: OffsetDateTime) -> RateStatus {
        let hour = hour_bucket(now);
        let day = now.date();
        let hourly_used = self
            .snapshot
            .hourly
            .as_ref()
            .filter(|window| window.start == hour)
            .map(|window| window.count)
            .unwrap_or(0);
        let hourly_remaining = config.hourly_quota.get().saturating_sub(hourly_used);
        let hourly_reset_at = hour.checked_add(TimeDuration::hours(1)).unwrap_or(now);

        let (daily_used, daily_remaining, daily_reset_at) = match config.daily_quota {
            Some(limit) => {
                let used = self
                    .snapshot
                    .daily
                    .as_ref()
                    .filter(|window| window.date == day)
                    .map(|window| window.count)
                    .unwrap_or(0);
                let remaining = limit.get().saturating_sub(used);
                let reset = day
                    .next_day()
                    .and_then(|d| d.with_hms(0, 0, 0).ok())
                    .map(|dt| dt.assume_utc());
                (Some(used), Some(remaining), reset)
            }
            None => (None, None, None),
        };

        RateStatus {
            hourly_remaining,
            hourly_limit: config.hourly_quota.get(),
            hourly_reset_at,
            hourly_used,
            daily_limit: config.daily_quota.map(|n| n.get()),
            daily_remaining,
            daily_used,
            daily_reset_at,
        }
    }
}

fn hour_bucket(now: OffsetDateTime) -> OffsetDateTime {
    now.replace_minute(0)
        .unwrap()
        .replace_second(0)
        .unwrap()
        .replace_nanosecond(0)
        .unwrap()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UsageSnapshot {
    account: String,
    #[serde(default)]
    hourly: Option<HourWindow>,
    #[serde(default)]
    daily: Option<DayWindow>,
}

impl UsageSnapshot {
    fn new(account: String) -> Self {
        Self {
            account,
            hourly: None,
            daily: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HourWindow {
    #[serde(with = "time::serde::rfc3339")]
    start: OffsetDateTime,
    count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DayWindow {
    #[serde(with = "serde_date")]
    date: Date,
    count: u32,
}

mod serde_date {
    use super::*;

    const FORMAT: &[FormatItem<'static>] = format_description!("[year]-[month]-[day]");

    pub fn serialize<S>(date: &Date, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let formatted = date.format(FORMAT).map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&formatted)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Date, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Date::parse(&value, FORMAT).map_err(serde::de::Error::custom)
    }
}

/// Snapshot of remaining budget for presentation.
#[derive(Debug, Clone, PartialEq)]
pub struct RateStatus {
    pub hourly_remaining: u32,
    pub hourly_limit: u32,
    pub hourly_used: u32,
    pub hourly_reset_at: OffsetDateTime,
    pub daily_limit: Option<u32>,
    pub daily_used: Option<u32>,
    pub daily_remaining: Option<u32>,
    pub daily_reset_at: Option<OffsetDateTime>,
}

#[derive(thiserror::Error, Debug)]
pub enum RateLimitError {
    #[error("I/O error at {path:?}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to deserialize usage file: {0}")]
    Deserialize(#[source] serde_json::Error),
    #[error("failed to serialize usage file: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigPaths;
    use std::num::NonZeroU32;
    use tempfile::tempdir;
    use tokio::time::Instant;

    fn temp_paths() -> (ConfigPaths, tempfile::TempDir) {
        let tmp = tempdir().expect("tempdir");
        let paths = ConfigPaths::from_base_dir(tmp.path());
        (paths, tmp)
    }

    #[tokio::test]
    async fn records_and_persists_usage() {
        let (paths, _guard) = temp_paths();
        let config = RateLimitConfig::new(NonZeroU32::new(10).unwrap());
        let limiter = RateLimiter::new(paths.clone(), "test", config)
            .await
            .expect("limiter");
        let now = OffsetDateTime::now_utc();
        let status = limiter.record_success(5, now).await.expect("record");
        assert_eq!(status.hourly_used, 5);
        assert_eq!(status.hourly_remaining, 5);

        // Reload from disk and ensure counts persisted.
        let limiter2 = RateLimiter::new(
            paths,
            "test",
            RateLimitConfig::new(NonZeroU32::new(10).unwrap()),
        )
        .await
        .expect("limiter2");
        let status2 = limiter2.status(now).await.expect("status");
        assert_eq!(status2.hourly_used, 5);
    }

    #[tokio::test]
    async fn hourly_window_resets_after_boundary() {
        let (paths, _guard) = temp_paths();
        let config = RateLimitConfig::new(NonZeroU32::new(10).unwrap());
        let limiter = RateLimiter::new(paths, "test", config)
            .await
            .expect("limiter");
        let now = OffsetDateTime::now_utc();
        limiter.record_success(10, now).await.expect("record");
        let next_hour = hour_bucket(now)
            .checked_add(TimeDuration::hours(1))
            .unwrap();
        let status = limiter.status(next_hour).await.expect("status");
        assert_eq!(status.hourly_used, 0);
        assert_eq!(status.hourly_remaining, 10);
    }

    #[tokio::test]
    async fn rate_limiter_waits_when_quota_exceeded() {
        let (paths, _guard) = temp_paths();
        let quota = Quota::per_second(NonZeroU32::new(1).unwrap());
        let config = RateLimitConfig::new(NonZeroU32::new(1).unwrap()).with_quota(quota);
        let limiter = RateLimiter::new(paths, "test", config)
            .await
            .expect("limiter");

        limiter.acquire(NonZeroU32::new(1).unwrap()).await;
        let start = Instant::now();
        limiter.acquire(NonZeroU32::new(1).unwrap()).await;
        let elapsed = start.elapsed();
        assert!(elapsed >= StdDuration::from_millis(900));
    }
}
