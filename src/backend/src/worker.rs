pub mod baidu;
mod queue;

use crate::{
    DATABASE,
    settings::Settings,
    storage::download_records::{Completion, CompletionReason},
};
use queue::Queue;
pub use queue::{QueueAction, QueueError, QueueRequest};
use serde::Serialize;
use std::{
    future::Future,
    io,
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{RwLock, mpsc, watch},
    task::JoinHandle,
    time::{Instant, sleep_until},
};

pub struct RemoteFile {
    pub id: String,
    pub path: String,
    pub size: u64,
    pub local_progress: Option<LocalProgress>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct LocalProgress {
    pub bytes: u64,
    pub total: u64,
    pub kind: LocalProgressKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalProgressKind {
    Checkpoint,
    LocalFile,
}

pub trait DownloadProvider: Send + Sync + 'static {
    const ID: &'static str;
    type Session: DownloadSession;

    fn ready(&self, settings: &Settings) -> bool;
    fn scan_interval(&self, settings: &Settings) -> Duration;
    fn recycle_on_remove(settings: &Settings) -> bool;
    fn connect(
        &self,
        settings: &Settings,
    ) -> impl Future<Output = io::Result<Self::Session>> + Send;
}

pub trait DownloadSession: Send + Sync {
    type Transfer: DownloadTransfer;

    fn account(&self) -> &str;
    fn current(&self, settings: &Settings) -> bool;
    fn contains(&self, path: &str) -> bool;
    fn list(&self) -> impl Future<Output = io::Result<Vec<RemoteFile>>> + Send;
    fn start(&self, file: &Download) -> impl Future<Output = io::Result<Self::Transfer>> + Send;
    fn recycle(
        &self,
        file: &Download,
        settings: &watch::Receiver<Settings>,
    ) -> impl Future<Output = io::Result<()>> + Send;
    fn after_complete(
        &self,
        transfer: &Self::Transfer,
        settings: &watch::Receiver<Settings>,
    ) -> impl Future<Output = io::Result<bool>> + Send;
}

pub trait DownloadTransfer: Send + Sync {
    fn next(&mut self) -> impl Future<Output = io::Result<TransferEvent>> + Send;
    fn stop(&mut self) -> impl Future<Output = io::Result<()>> + Send;
}

pub enum TransferEvent {
    Progress(TransferProgress),
    Finished(TransferProgress, Option<FailureReason>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "code", rename_all = "snake_case")]
pub enum FailureReason {
    Timeout,
    TlsHandshakeTimeout,
    Network,
    MissingFile,
    InvalidResponse,
    FileChanged,
    PermissionDenied,
    ChecksumMismatch,
    LoginRequired,
    ProviderError(i32),
    NoCompletion,
    Unexpected,
}

impl std::fmt::Display for FailureReason {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProviderError(code) => write!(out, "Provider error code {code}"),
            other => write!(out, "{other:?}"),
        }
    }
}

impl std::error::Error for FailureReason {}

impl From<&io::Error> for FailureReason {
    fn from(error: &io::Error) -> Self {
        if let Some(reason) = error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<Self>())
        {
            return *reason;
        }
        match error.kind() {
            io::ErrorKind::TimedOut => Self::Timeout,
            io::ErrorKind::InvalidData => Self::InvalidResponse,
            io::ErrorKind::NotFound => Self::MissingFile,
            io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::NotConnected => Self::Network,
            _ => Self::Unexpected,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    Queued,
    Active,
    Retrying,
    Paused,
    Complete,
    Existing,
    Failed,
}

#[derive(Clone, Copy, Serialize)]
pub struct TransferProgress {
    pub status: DownloadState,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub speed: u64,
}

#[derive(Clone, Serialize)]
pub struct Download {
    pub id: String,
    pub provider: &'static str,
    pub path: String,
    #[serde(flatten)]
    pub progress: TransferProgress,
    pub last_failure: Option<FailureReason>,
    pub local_progress: Option<LocalProgress>,
    pub completion: Option<Completion>,
    #[serde(skip)]
    pub remote_id: String,
}

#[derive(Default)]
pub struct Snapshot {
    pub phase: &'static str,
    account: String,
    files: Vec<Download>,
    history: Vec<Download>,
    next_scan: Option<Instant>,
}

impl Snapshot {
    pub fn next_check_at(&self) -> Option<u64> {
        let deadline = self.next_scan?;
        let now = Instant::now();
        let wall_clock = SystemTime::now();
        let next = match deadline.checked_duration_since(now) {
            Some(delay) => wall_clock.checked_add(delay)?,
            None => wall_clock.checked_sub(now.duration_since(deadline))?,
        };
        Some(next.duration_since(UNIX_EPOCH).ok()?.as_secs())
    }

    pub fn files(&self) -> Vec<Download> {
        let mut files = self.files.clone();
        files.sort_by_key(|file| match file.progress.status {
            DownloadState::Active | DownloadState::Retrying => 0,
            DownloadState::Queued => 1,
            DownloadState::Paused => 2,
            _ => 3,
        });
        files
    }

    pub fn completed_files(&self) -> Vec<Download> {
        let mut files = self.history.clone();
        files.sort_by(|a, b| {
            b.completion
                .map(|entry| entry.at)
                .cmp(&a.completion.map(|entry| entry.at))
                .then_with(|| a.path.cmp(&b.path))
        });
        files
    }

    fn file_mut(&mut self, id: &str) -> Option<&mut Download> {
        self.files.iter_mut().find(|file| file.id == id)
    }
}

async fn schedule_scan(status: &RwLock<Snapshot>, interval: Duration) -> Instant {
    let at = Instant::now() + interval;
    status.write().await.next_scan = Some(at);
    at
}

pub struct WorkerHandle {
    pub status: Arc<RwLock<Snapshot>>,
    pub requests: mpsc::Sender<QueueRequest>,
    pub recycle_on_remove: fn(&Settings) -> bool,
}

pub fn spawn<P: DownloadProvider>(
    provider: P,
    settings: watch::Receiver<Settings>,
) -> (WorkerHandle, JoinHandle<()>) {
    let (sender, receiver) = mpsc::channel(16);
    let status = Arc::new(RwLock::new(Snapshot::default()));
    let handle = WorkerHandle {
        status: status.clone(),
        requests: sender,
        recycle_on_remove: P::recycle_on_remove,
    };
    let worker = tokio::spawn(async move {
        run(provider, settings, &status, receiver, Path::new(DATABASE)).await
    });
    (handle, worker)
}

async fn download<P: DownloadProvider>(
    file: &Download,
    queue: &Queue<'_, P>,
    requests: &mut mpsc::Receiver<QueueRequest>,
    next_scan: &mut Instant,
) -> io::Result<()> {
    let mut transfer = queue.session.start(file).await?;
    let refresh_at = |deadline| async move {
        sleep_until(deadline).await;
        queue.session.list().await
    };
    let scan = refresh_at(*next_scan);
    tokio::pin!(scan);
    loop {
        let event = tokio::select! {
            request = requests.recv() => {
                let Some(request) = request else { return Ok(()); };
                if queue.handle(request, Some((&file.id, &mut transfer))).await {
                    return Ok(());
                }
                continue;
            }
            result = &mut scan => {
                *next_scan = schedule_scan(queue.status, queue.interval).await;
                scan.set(refresh_at(*next_scan));
                let result = match result {
                    Ok(files) if queue.session.current(&queue.settings.borrow()) => queue.reconcile(files, true).await,
                    Ok(_) => continue,
                    Err(error) => Err(error),
                };
                match result {
                    Ok(()) if !queue.status.read().await.files.iter().any(|row| row.id == file.id) => {
                        transfer.stop().await?;
                        return Ok(());
                    }
                    Ok(()) => {}
                    Err(error) => eprintln!("{} queue refresh failed: {error}", P::ID),
                }
                continue;
            }
            event = transfer.next() => event?,
        };
        let (progress, finished, failure) = match event {
            TransferEvent::Progress(progress) => (progress, false, None),
            TransferEvent::Finished(progress, failure) => (progress, true, failure),
        };
        if finished
            && matches!(
                progress.status,
                DownloadState::Complete | DownloadState::Existing
            )
        {
            let reason = if progress.status == DownloadState::Complete {
                CompletionReason::Downloaded
            } else {
                CompletionReason::AlreadyLocal
            };
            queue.archive(file, progress, reason).await?;
        } else if let Some(row) = queue.status.write().await.file_mut(&file.id) {
            row.progress = progress;
            if finished {
                row.last_failure = failure.or_else(|| {
                    (progress.status == DownloadState::Failed)
                        .then_some(FailureReason::NoCompletion)
                });
            }
        }
        if finished {
            if progress.status == DownloadState::Complete {
                match queue
                    .session
                    .after_complete(&transfer, queue.settings)
                    .await
                {
                    Ok(true) => eprintln!("{} moved {:?} to the recycle bin", P::ID, file.path),
                    Ok(false) => {}
                    Err(error) => eprintln!("{} could not recycle {:?}: {error}", P::ID, file.path),
                }
            }
            return Ok(());
        }
    }
}

async fn run<P: DownloadProvider>(
    provider: P,
    mut settings: watch::Receiver<Settings>,
    status: &RwLock<Snapshot>,
    mut requests: mpsc::Receiver<QueueRequest>,
    database: &Path,
) {
    loop {
        let current = settings.borrow_and_update().clone();
        let interval = provider.scan_interval(&current);
        let session = if provider.ready(&current) {
            {
                let mut snapshot = status.write().await;
                snapshot.phase = "checking";
                snapshot.next_scan = Some(Instant::now());
            }
            match provider.connect(&current).await {
                Ok(session) => Some(session),
                Err(error) => {
                    eprintln!("{} connection failed: {error}", P::ID);
                    status.write().await.phase = "login_failed";
                    None
                }
            }
        } else {
            *status.write().await = Snapshot {
                phase: "setup",
                ..Snapshot::default()
            };
            None
        };
        let queue = session.as_ref().map(|session| Queue::<P> {
            session,
            settings: &settings,
            status,
            interval,
            database,
        });
        let mut next_scan;
        if let Some(queue) = &queue {
            let mut scan_result = match queue.prepare_account().await {
                Ok(()) => queue.scan(&mut requests, false).await,
                Err(error) => Err(error),
            };
            next_scan = schedule_scan(status, interval).await;
            while scan_result.is_ok() && !settings.has_changed().unwrap_or(true) {
                while let Ok(request) = requests.try_recv() {
                    queue.handle(request, None).await;
                }
                if Instant::now() >= next_scan {
                    scan_result = queue.scan(&mut requests, true).await;
                    next_scan = schedule_scan(status, interval).await;
                    if scan_result.is_err() {
                        break;
                    }
                }
                let file = {
                    let mut snapshot = status.write().await;
                    snapshot
                        .files
                        .iter_mut()
                        .find(|file| file.progress.status == DownloadState::Queued)
                        .map(|file| {
                            file.progress.status = DownloadState::Active;
                            file.clone()
                        })
                };
                let Some(file) = file else {
                    break;
                };
                status.write().await.phase = "downloading";
                if let Err(error) = download(&file, queue, &mut requests, &mut next_scan).await {
                    eprintln!("{} download failed for {:?}: {error}", P::ID, file.path);
                    if let Some(row) = status.write().await.file_mut(&file.id) {
                        row.progress.status = DownloadState::Failed;
                        row.progress.speed = 0;
                        row.last_failure = Some(FailureReason::from(&error));
                    }
                }
            }
            let mut snapshot = status.write().await;
            if let Err(error) = &scan_result {
                eprintln!("{} queue scan failed: {error}", P::ID);
            }
            snapshot.phase = if scan_result.is_err()
                || snapshot
                    .files
                    .iter()
                    .any(|row| row.progress.status == DownloadState::Failed)
            {
                "download_failed"
            } else {
                "idle"
            };
        } else {
            next_scan = schedule_scan(status, interval).await;
        }
        let wait = sleep_until(next_scan);
        tokio::pin!(wait);
        loop {
            // Construct the controller only while processing a command so the watch
            // receiver can be borrowed mutably for changes while idle.
            tokio::select! {
                changed = settings.changed() => {
                    if changed.is_err() { return; }
                    break;
                }
                () = &mut wait => break,
                request = requests.recv() => {
                    let Some(request) = request else { return; };
                    let resume = request.action == QueueAction::Resume;
                    if let Some(session) = &session {
                        let queue = Queue::<P> { session, settings: &settings, status, interval, database };
                        queue.handle(request, None).await;
                        if resume && status.read().await.files.iter().any(|file| file.progress.status == DownloadState::Queued) {
                            break;
                        }
                    } else {
                        let _ = request.reply.send(Err(QueueError::Unavailable));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/backend/src/worker.rs"]
mod tests;
