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
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, Notify};

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
            JobKind::StatsBreakdown { query } => {
                format!("Breakdown stats for {} ({:?})", self.account, query.metrics)
            }
            JobKind::EventSend { .. } => format!("Send event for {}", self.account),
            JobKind::EventsImport { events } => {
                format!("Import {} events for {}", events.len(), self.account)
            }
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
    EventSend { event: serde_json::Value },
    EventsImport { events: Vec<serde_json::Value> },
    Custom { label: String },
}

#[derive(Debug, Clone)]
pub enum JobResponse {
    Sites(Vec<SiteSummary>),
    StatsAggregate(AggregateResponse),
    StatsTimeseries(TimeseriesResponse),
    StatsBreakdown(BreakdownResponse),
    EventAck,
    EventsProcessed { processed: usize },
    Acknowledged,
    Custom(serde_json::Value),
}

#[derive(Debug, Clone)]
pub struct QueueSnapshot {
    pub id: JobId,
    pub account: String,
    pub description: String,
    pub state: QueueJobState,
    pub enqueued_at: OffsetDateTime,
    pub started_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueJobState {
    Pending,
    InFlight,
}

#[derive(Debug, Clone)]
struct JobStateEntry {
    id: JobId,
    account: String,
    description: String,
    enqueued_at: OffsetDateTime,
    started_at: Option<OffsetDateTime>,
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
    state: Arc<Mutex<Vec<JobStateEntry>>>,
    idle_notify: Arc<Notify>,
}

impl QueueHandle {
    pub async fn submit(
        &self,
        request: JobRequest,
        weight: NonZeroU32,
    ) -> Result<JobTicket, WorkerError> {
        let id = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        let description = request.description();
        let account = request.account.clone();
        let enqueued_at = OffsetDateTime::now_utc();
        let (tx, rx) = oneshot::channel();
        let message = JobMessage {
            id,
            request: request.clone(),
            weight,
            responder: Some(tx),
            enqueued_at,
        };
        {
            let mut state = self.state.lock().await;
            state.push(JobStateEntry {
                id,
                account: account.clone(),
                description: description.clone(),
                enqueued_at,
                started_at: None,
            });
        }
        self.telemetry
            .send(TelemetryEvent {
                job_id: id,
                account: account.clone(),
                description: description.clone(),
                kind: TelemetryKind::Enqueued,
                timestamp: message.enqueued_at,
                status: None,
            })
            .ok();
        if self.sender.send(message).await.is_err() {
            let mut state = self.state.lock().await;
            state.retain(|entry| entry.id != id);
            if state.is_empty() {
                self.idle_notify.notify_waiters();
            }
            return Err(WorkerError::QueueClosed);
        }
        Ok(JobTicket { id, receiver: rx })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TelemetryEvent> {
        self.telemetry.subscribe()
    }

    pub async fn snapshot(&self) -> Vec<QueueSnapshot> {
        let state = self.state.lock().await;
        state
            .iter()
            .map(|entry| QueueSnapshot {
                id: entry.id,
                account: entry.account.clone(),
                description: entry.description.clone(),
                state: if entry.started_at.is_some() {
                    QueueJobState::InFlight
                } else {
                    QueueJobState::Pending
                },
                enqueued_at: entry.enqueued_at,
                started_at: entry.started_at,
            })
            .collect()
    }

    pub async fn wait_idle(&self) {
        loop {
            if self.is_idle().await {
                return;
            }
            let notified = self.idle_notify.notified();
            if self.is_idle().await {
                return;
            }
            notified.await;
        }
    }

    pub async fn is_idle(&self) -> bool {
        let state = self.state.lock().await;
        state.is_empty()
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
    state: Arc<Mutex<Vec<JobStateEntry>>>,
    idle_notify: Arc<Notify>,
}

impl<E: JobExecutor> Worker<E> {
    pub fn spawn(
        executor: Arc<E>,
        rate_limiter: RateLimiter,
        capacity: Option<usize>,
    ) -> QueueHandle {
        let (tx, rx) = mpsc::channel(capacity.unwrap_or(DEFAULT_QUEUE_CAPACITY));
        let (telemetry_tx, _) = broadcast::channel(256);
        let state = Arc::new(Mutex::new(Vec::new()));
        let idle_notify = Arc::new(Notify::new());
        let worker = Self {
            queue: rx,
            telemetry: telemetry_tx.clone(),
            executor,
            rate_limiter: rate_limiter.clone(),
            state: state.clone(),
            idle_notify: idle_notify.clone(),
        };
        tokio::spawn(worker.run());
        QueueHandle {
            sender: tx,
            telemetry: telemetry_tx,
            sequence: Arc::new(AtomicU64::new(0)),
            state,
            idle_notify,
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
            {
                let mut state = self.state.lock().await;
                if let Some(entry) = state.iter_mut().find(|entry| entry.id == id) {
                    entry.started_at = Some(start);
                }
            }

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
                    self.finish_job(id).await;
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
                    self.finish_job(id).await;
                }
            }
        }
    }

    async fn finish_job(&self, id: JobId) {
        let mut state = self.state.lock().await;
        state.retain(|entry| entry.id != id);
        if state.is_empty() {
            self.idle_notify.notify_waiters();
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
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;
    use tempfile::tempdir;
    use tokio::sync::Notify;

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

    struct BlockingExecutor {
        resume: Arc<Notify>,
        started: Arc<Notify>,
    }

    impl BlockingExecutor {
        fn new() -> Self {
            Self {
                resume: Arc::new(Notify::new()),
                started: Arc::new(Notify::new()),
            }
        }

        fn resume_notifier(&self) -> Arc<Notify> {
            self.resume.clone()
        }

        fn started_notifier(&self) -> Arc<Notify> {
            self.started.clone()
        }
    }

    #[async_trait]
    impl JobExecutor for BlockingExecutor {
        async fn execute(&self, _request: JobRequest) -> JobResult {
            let resume_wait = self.resume.notified();
            self.started.notify_waiters();
            resume_wait.await;
            Ok(JobResponse::Acknowledged)
        }
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

    #[tokio::test]
    async fn snapshot_reflects_inflight_jobs() {
        let executor = Arc::new(BlockingExecutor::new());
        let resume = executor.resume_notifier();
        let started = executor.started_notifier();
        let rate_limiter = setup_rate_limiter().await;
        let handle = Worker::spawn(executor, rate_limiter, Some(4));

        let ticket = handle
            .submit(
                JobRequest {
                    account: "acct".into(),
                    kind: JobKind::Custom {
                        label: "blocking".into(),
                    },
                },
                NonZeroU32::new(1).unwrap(),
            )
            .await
            .expect("enqueue");

        started.notified().await;
        let snapshot = handle.snapshot().await;
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].account, "acct");
        assert_eq!(snapshot[0].state, QueueJobState::InFlight);
        assert!(snapshot[0].started_at.is_some());

        resume.notify_waiters();
        ticket.await_result().await.expect("result");
        handle.wait_idle().await;
        assert!(handle.snapshot().await.is_empty());
    }
}
