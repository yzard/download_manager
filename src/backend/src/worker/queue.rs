use super::{
    Download, DownloadProvider, DownloadSession, DownloadState, DownloadTransfer, RemoteFile,
    Snapshot, TransferProgress,
};
use crate::{
    settings::Settings,
    storage::download_queue::{self, Disposition},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, io, path::Path, time::Duration};
use tokio::sync::{RwLock, mpsc, oneshot, watch};

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueueAction {
    Pause,
    Resume,
    Remove,
}

pub enum QueueError {
    NotFound,
    Conflict,
    RecycleFailed,
    Unavailable,
}

pub struct QueueRequest {
    pub id: String,
    pub action: QueueAction,
    pub recycle: bool,
    pub reply: oneshot::Sender<Result<(), QueueError>>,
}

pub(super) struct Queue<'a, P: DownloadProvider> {
    pub session: &'a P::Session,
    pub settings: &'a watch::Receiver<Settings>,
    pub status: &'a RwLock<Snapshot>,
    pub interval: Duration,
    pub database: &'a Path,
}

fn task_id(provider: &str, account: &str, file: &RemoteFile) -> String {
    let mut hash = Sha256::new();
    for part in [provider, account, &file.id, &file.path] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    format!("{:x}", hash.finalize())
}

impl<P: DownloadProvider> Queue<'_, P> {
    pub(super) async fn scan(
        &self,
        requests: &mut mpsc::Receiver<QueueRequest>,
        preserve: bool,
    ) -> io::Result<()> {
        let scan = self.session.list();
        tokio::pin!(scan);
        let files = loop {
            tokio::select! {
                result = &mut scan => break result?,
                request = requests.recv() => {
                    let request = request.ok_or_else(|| io::Error::other("Download queue closed"))?;
                    self.handle(request, None).await;
                }
            }
        };
        if self.session.current(&self.settings.borrow()) {
            self.reconcile(files, preserve).await?;
        }
        Ok(())
    }

    pub(super) async fn reconcile(
        &self,
        mut files: Vec<RemoteFile>,
        preserve: bool,
    ) -> io::Result<()> {
        let account = self.session.account().to_owned();
        let database = self.database.to_owned();
        let dispositions = tokio::task::spawn_blocking({
            let account = account.clone();
            move || download_queue::load(&database, P::ID, &account)
        })
        .await
        .map_err(io::Error::other)??;
        let mut paths = std::collections::HashSet::with_capacity(files.len());
        if files.iter().any(|file| {
            file.id.is_empty() || !self.session.contains(&file.path) || !paths.insert(&file.path)
        }) {
            return Err(io::Error::other("Invalid or duplicate cloud file identity"));
        }
        files.sort_by(|a, b| a.size.cmp(&b.size).then_with(|| a.path.cmp(&b.path)));
        let mut snapshot = self.status.write().await;
        let mut old: HashMap<_, _> = snapshot.files.iter().map(|file| (&file.id, file)).collect();
        let next = files
            .into_iter()
            .filter_map(|file| {
                let id = task_id(P::ID, &account, &file);
                let prior = old.remove(&id);
                let disposition = dispositions.get(&(file.id.clone(), file.path.clone()));
                if disposition == Some(&Disposition::Removed) {
                    return None;
                }
                let progress = match (disposition, prior) {
                    (Some(Disposition::Paused), prior) => TransferProgress {
                        status: DownloadState::Paused,
                        speed: 0,
                        total: Some(file.size),
                        downloaded: prior.map_or(0, |row| row.progress.downloaded),
                    },
                    (_, Some(row)) if preserve => row.progress,
                    _ => TransferProgress {
                        status: DownloadState::Queued,
                        downloaded: 0,
                        total: Some(file.size),
                        speed: 0,
                    },
                };
                Some(Download {
                    id,
                    provider: P::ID,
                    path: file.path,
                    remote_id: file.id,
                    progress,
                })
            })
            .collect();
        snapshot.account = account;
        snapshot.files = next;
        Ok(())
    }

    fn matches_settings(&self, recycle: Option<bool>) -> bool {
        let latest = self.settings.borrow();
        self.session.current(&latest)
            && recycle.is_none_or(|requested| requested == P::recycle_on_remove(&latest))
    }

    async fn persist(&self, file: &Download, state: Option<Disposition>) -> Result<(), QueueError> {
        let account = self.session.account().to_owned();
        let file_id = file.remote_id.clone();
        let path = file.path.clone();
        let database = self.database.to_owned();
        tokio::task::spawn_blocking(move || {
            download_queue::save(&database, P::ID, &account, &file_id, &path, state)
        })
        .await
        .map_err(io::Error::other)
        .and_then(|result| result)
        .map_err(|error| {
            eprintln!("{} queue state could not be saved: {error}", P::ID);
            QueueError::Unavailable
        })
    }

    pub(super) async fn handle(
        &self,
        request: QueueRequest,
        active: Option<(&str, &mut <P::Session as DownloadSession>::Transfer)>,
    ) -> bool {
        let mut interrupted = false;
        let result = self.apply(&request, active, &mut interrupted).await;
        let _ = request.reply.send(result);
        interrupted
    }

    async fn apply(
        &self,
        request: &QueueRequest,
        active: Option<(&str, &mut <P::Session as DownloadSession>::Transfer)>,
        interrupted: &mut bool,
    ) -> Result<(), QueueError> {
        let recycle = (request.action == QueueAction::Remove).then_some(request.recycle);
        if !self.matches_settings(recycle) {
            return Err(QueueError::Conflict);
        }
        let file = {
            let snapshot = self.status.read().await;
            if snapshot.account != self.session.account() {
                return Err(QueueError::NotFound);
            }
            snapshot
                .files
                .iter()
                .find(|file| file.id == request.id && self.session.contains(&file.path))
                .cloned()
                .ok_or(QueueError::NotFound)?
        };
        match request.action {
            QueueAction::Resume if file.progress.status != DownloadState::Paused => {
                return Err(QueueError::Conflict);
            }
            QueueAction::Pause
                if !matches!(
                    file.progress.status,
                    DownloadState::Queued
                        | DownloadState::Active
                        | DownloadState::Retrying
                        | DownloadState::Failed
                        | DownloadState::Paused
                ) =>
            {
                return Err(QueueError::Conflict);
            }
            _ => {}
        }
        if request.action == QueueAction::Resume {
            self.persist(&file, None).await?;
            if let Some(row) = self.status.write().await.file_mut(&file.id) {
                row.progress.status = DownloadState::Queued;
            }
            return Ok(());
        }

        let recycle = request.recycle && request.action == QueueAction::Remove;
        let disposition = if request.action == QueueAction::Remove && !recycle {
            Disposition::Removed
        } else {
            Disposition::Paused
        };
        self.persist(&file, Some(disposition)).await?;
        if let Some(row) = self.status.write().await.file_mut(&file.id) {
            row.progress.status = DownloadState::Paused;
            row.progress.speed = 0;
        }
        if let Some((_, transfer)) = active.filter(|(id, _)| *id == file.id) {
            *interrupted = true;
            transfer.stop().await.map_err(|error| {
                eprintln!("{} transfer could not be stopped: {error}", P::ID);
                QueueError::Unavailable
            })?;
        }
        if recycle {
            self.session
                .recycle(&file, self.settings)
                .await
                .map_err(|error| {
                    eprintln!(
                        "{} could not recycle removed task {:?}: {error}",
                        P::ID,
                        file.path
                    );
                    QueueError::RecycleFailed
                })?;
            self.persist(&file, Some(Disposition::Removed)).await?;
        }
        if request.action == QueueAction::Remove {
            self.status
                .write()
                .await
                .files
                .retain(|row| row.id != file.id);
        }
        Ok(())
    }
}
