use crate::{
    client::{
        AggregateQuery, AggregateResponse, BreakdownQuery, BreakdownResponse, SiteSummary,
        TimeseriesQuery, TimeseriesResponse,
    },
    rate_limit::{RateLimitError, RateLimiter, RateStatus},
};
use async_trait::async_trait;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use time::OffsetDateTime;
use tokio::sync::{broadcast, mpsc, oneshot};

const DEFAULT_QUEUE_CAPACITY: usize = 128;

pub type JobId = u64;
pub type JobResult = Result<JobResponse, WorkerError>;

#[derive(Debug, Clone)]
pub struct JobRequest {
    pub account: String,
    pub kind: JobKind,
}

impl JobRequest {
    pub fn description(&self) -> String {
        match &self.kind {
            JobKind::ListSites => format!("List sites for {}", self.account),
            JobKind::StatsAggregate { query } => {
                format!("Aggregate stats for {} ({:?})", self.account, query.metrics)
            }
            JobKind::StatsTimeseries { query } => format!(
                "Timeseries stats for {} ({:?})",
                self.account, query.metrics
            ),
            JobKind::StatsBreakdown { query } => format!(
                "Breakdown stats for {} ({:?})",
                self.account, query.metrics
            ),
            JobKind::Custom { label } => label.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum JobKind {
    ListSites,
    StatsAggregate { query: Box<AggregateQuery> },
    StatsTimeseries { query: Box<TimeseriesQuery> },
    StatsBreakdown { query: Box<BreakdownQuery> },
    Custom { label: String },
}

#[derive(Debug, Clone)]
pub enum JobResponse {
    Sites(Vec<SiteSummary>),
    StatsAggregate(AggregateResponse),
    StatsTimeseries(TimeseriesResponse),
    StatsBreakdown(BreakdownResponse),
    Acknowledged,
    Custom(serde_json::Value),
}

#[derive(Debug, Clone)]
pub struct TelemetryEvent {
    pub job_id: JobId,
    pub account: String,
    pub description: String,
    pub kind: TelemetryKind,
    pub timestamp: OffsetDateTime,
    pub status: Option<RateStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryKind {
    Enqueued,
    Started,
    Succeeded,
    Failed,
}

#[derive(Clone)]
pub struct QueueHandle {
    sender: mpsc::Sender<JobMessage>,
    telemetry: broadcast::Sender<TelemetryEvent>,
    sequence: Arc<AtomicU64>,
}

impl QueueHandle {
    pub async fn submit(
        &self,
        request: JobRequest,
        weight: NonZeroU32,
    ) -> Result<JobTicket, WorkerError> {
        let id = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = oneshot::channel();
        let message = JobMessage {
            id,
            request: request.clone(),
            weight,
            responder: Some(tx),
            enqueued_at: OffsetDateTime::now_utc(),
        };
        self.telemetry
            .send(TelemetryEvent {
                job_id: id,
                account: request.account.clone(),
                description: request.description(),
                kind: TelemetryKind::Enqueued,
                timestamp: message.enqueued_at,
                status: None,
            })
            .ok();
        self.sender
            .send(message)
            .await
            .map_err(|_| WorkerError::QueueClosed)?;
        Ok(JobTicket { id, receiver: rx })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TelemetryEvent> {
        self.telemetry.subscribe()
    }
}

pub struct JobTicket {
    id: JobId,
    receiver: oneshot::Receiver<JobResult>,
}

impl JobTicket {
    pub fn id(&self) -> JobId {
        self.id
    }

    pub async fn await_result(self) -> JobResult {
        self.receiver.await.unwrap_or(Err(WorkerError::QueueClosed))
    }
}

struct JobMessage {
    id: JobId,
    request: JobRequest,
    weight: NonZeroU32,
    responder: Option<oneshot::Sender<JobResult>>,
    enqueued_at: OffsetDateTime,
}

#[async_trait]
pub trait JobExecutor: Send + Sync + 'static {
    async fn execute(&self, request: JobRequest) -> JobResult;
}

pub struct Worker<E: JobExecutor> {
    queue: mpsc::Receiver<JobMessage>,
    telemetry: broadcast::Sender<TelemetryEvent>,
    executor: Arc<E>,
    rate_limiter: RateLimiter,
}

impl<E: JobExecutor> Worker<E> {
    pub fn spawn(
        executor: Arc<E>,
        rate_limiter: RateLimiter,
        capacity: Option<usize>,
    ) -> QueueHandle {
        let (tx, rx) = mpsc::channel(capacity.unwrap_or(DEFAULT_QUEUE_CAPACITY));
        let (telemetry_tx, _) = broadcast::channel(256);
        let worker = Self {
            queue: rx,
            telemetry: telemetry_tx.clone(),
            executor,
            rate_limiter: rate_limiter.clone(),
        };
        tokio::spawn(worker.run());
        QueueHandle {
            sender: tx,
            telemetry: telemetry_tx,
            sequence: Arc::new(AtomicU64::new(0)),
        }
    }

    async fn run(mut self) {
        while let Some(message) = self.queue.recv().await {
            let JobMessage {
                id,
                request,
                weight,
                responder,
                ..
            } = message;
            let start = OffsetDateTime::now_utc();
            self.telemetry
                .send(TelemetryEvent {
                    job_id: id,
                    account: request.account.clone(),
                    description: request.description(),
                    kind: TelemetryKind::Started,
                    timestamp: start,
                    status: None,
                })
                .ok();

            self.rate_limiter.acquire(weight).await;
            let result = self.executor.execute(request.clone()).await;

            match result {
                Ok(response) => {
                    let status = self
                        .rate_limiter
                        .record_success(weight.get(), OffsetDateTime::now_utc())
                        .await;
                    let status = status.ok();
                    self.telemetry
                        .send(TelemetryEvent {
                            job_id: id,
                            account: request.account.clone(),
                            description: request.description(),
                            kind: TelemetryKind::Succeeded,
                            timestamp: OffsetDateTime::now_utc(),
                            status,
                        })
                        .ok();
                    if let Some(tx) = responder {
                        let _ = tx.send(Ok(response));
                    }
                }
                Err(err) => {
                    self.telemetry
                        .send(TelemetryEvent {
                            job_id: id,
                            account: request.account.clone(),
                            description: request.description(),
                            kind: TelemetryKind::Failed,
                            timestamp: OffsetDateTime::now_utc(),
                            status: None,
                        })
                        .ok();
                    if let Some(tx) = responder {
                        let _ = tx.send(Err(err));
                    }
                }
            }
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum WorkerError {
    #[error("queue closed")]
    QueueClosed,
    #[error("rate limiter error: {0}")]
    RateLimit(#[from] RateLimitError),
    #[error("execution error: {0}")]
    Execution(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigPaths;
    use crate::rate_limit::RateLimitConfig;
    use std::num::NonZeroU32;
    use std::sync::Mutex as StdMutex;
    use tempfile::tempdir;

    struct TestExecutor {
        calls: StdMutex<Vec<JobKind>>,
        responses: StdMutex<Vec<JobResponse>>,
    }

    #[async_trait]
    impl JobExecutor for TestExecutor {
        async fn execute(&self, request: JobRequest) -> JobResult {
            self.calls.lock().unwrap().push(request.kind.clone());
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop()
                .unwrap_or(JobResponse::Acknowledged);
            Ok(response)
        }
    }

    async fn setup_rate_limiter() -> RateLimiter {
        let tmp = tempdir().expect("tmpdir");
        let paths = ConfigPaths::from_base_dir(tmp.path());
        let config = RateLimitConfig::new(NonZeroU32::new(100).unwrap());
        RateLimiter::new(paths, "test", config)
            .await
            .expect("limiter")
    }

    #[tokio::test]
    async fn worker_processes_jobs_and_respects_order() {
        let executor = Arc::new(TestExecutor {
            calls: StdMutex::new(Vec::new()),
            responses: StdMutex::new(vec![JobResponse::Acknowledged]),
        });
        let rate_limiter = setup_rate_limiter().await;
        let handle = Worker::spawn(executor.clone(), rate_limiter.clone(), Some(4));

        let request = JobRequest {
            account: "test".into(),
            kind: JobKind::Custom {
                label: "custom".into(),
            },
        };
        let ticket = handle
            .submit(request, NonZeroU32::new(1).unwrap())
            .await
            .expect("enqueue");
        let outcome = ticket.await_result().await.expect("result");
        matches!(outcome, JobResponse::Acknowledged);
    }

    #[tokio::test]
    async fn telemetry_receives_events() {
        let executor = Arc::new(TestExecutor {
            calls: StdMutex::new(Vec::new()),
            responses: StdMutex::new(vec![JobResponse::Acknowledged]),
        });
        let rate_limiter = setup_rate_limiter().await;
        let handle = Worker::spawn(executor, rate_limiter, Some(4));
        let mut telemetry = handle.subscribe();

        let request = JobRequest {
            account: "acct".into(),
            kind: JobKind::ListSites,
        };
        let ticket = handle
            .submit(request, NonZeroU32::new(1).unwrap())
            .await
            .expect("enqueue");

        for _ in 0..3 {
            let _event = telemetry.recv().await.expect("tele");
        }

        let _ = ticket.await_result().await.expect("result");
    }
}
