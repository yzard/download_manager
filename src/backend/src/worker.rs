pub mod baidu;
mod queue;

use crate::{DATABASE, settings::Settings};
use queue::Queue;
pub use queue::{QueueAction, QueueError, QueueRequest};
use serde::Serialize;
use std::{future::Future, io, path::Path, sync::Arc, time::Duration};
use tokio::{
    sync::{RwLock, mpsc, watch},
    task::JoinHandle,
    time::{Instant, sleep_until},
};

pub struct RemoteFile {
    pub id: String,
    pub path: String,
    pub size: u64,
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
    Finished(TransferProgress),
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
    #[serde(skip)]
    pub remote_id: String,
}

#[derive(Default)]
pub struct Snapshot {
    pub phase: &'static str,
    account: String,
    files: Vec<Download>,
}

impl Snapshot {
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

    fn file_mut(&mut self, id: &str) -> Option<&mut Download> {
        self.files.iter_mut().find(|file| file.id == id)
    }
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
                *next_scan = Instant::now() + queue.interval;
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
        let (progress, finished) = match event {
            TransferEvent::Progress(progress) => (progress, false),
            TransferEvent::Finished(progress) => (progress, true),
        };
        if let Some(row) = queue.status.write().await.file_mut(&file.id) {
            row.progress = progress;
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
            status.write().await.phase = "checking";
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
        let mut next_scan = Instant::now() + interval;
        if let Some(queue) = &queue {
            {
                let mut snapshot = status.write().await;
                if snapshot.account != queue.session.account() {
                    snapshot.files.clear();
                } else {
                    snapshot
                        .files
                        .retain(|file| queue.session.contains(&file.path));
                }
            }
            let mut scan_result = queue.scan(&mut requests, false).await;
            next_scan = Instant::now() + interval;
            while scan_result.is_ok() && !settings.has_changed().unwrap_or(true) {
                while let Ok(request) = requests.try_recv() {
                    queue.handle(request, None).await;
                }
                if Instant::now() >= next_scan {
                    scan_result = queue.scan(&mut requests, true).await;
                    next_scan = Instant::now() + interval;
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
