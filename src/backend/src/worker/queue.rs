use super::{
    Download, DownloadProvider, DownloadSession, DownloadState, DownloadTransfer,
    LocalProgressKind, RemoteFile, Snapshot, TransferProgress,
};
use crate::{
    auth,
    settings::Settings,
    storage::{
        download_queue::{self, Disposition},
        download_records::{self, Completion, CompletionReason, Record, Records},
    },
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    io,
    path::Path,
    time::Duration,
};
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

fn task_id(provider: &str, account: &str, file: &str, path: &str, size: u64) -> String {
    let mut hash = Sha256::new();
    for part in [provider, account, file, path] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    hash.update(size.to_be_bytes());
    format!("{:x}", hash.finalize())
}

fn saved_bytes(file: &Download) -> u64 {
    file.local_progress
        .filter(|local| local.kind == LocalProgressKind::Checkpoint)
        .map_or(file.progress.downloaded, |local| {
            local.bytes.max(file.progress.downloaded)
        })
}

fn history_file(
    provider: &'static str,
    account: &str,
    file_id: String,
    path: String,
    source_size: u64,
    record: Record,
) -> Download {
    Download {
        id: task_id(provider, account, &file_id, &path, source_size),
        provider,
        path,
        remote_id: file_id,
        source_size,
        progress: TransferProgress {
            status: DownloadState::Complete,
            downloaded: record.downloaded,
            total: Some(record.total),
            speed: 0,
        },
        last_failure: None,
        local_progress: None,
        completion: record.completion,
    }
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

    pub(super) async fn prepare_account(&self) -> io::Result<()> {
        let account = self.session.account().to_owned();
        {
            let mut snapshot = self.status.write().await;
            if snapshot.account == account {
                snapshot
                    .files
                    .retain(|file| self.session.contains(&file.path));
                return Ok(());
            }
            snapshot.account.clear();
            snapshot.files.clear();
            snapshot.history.clear();
        }
        let database = self.database.to_owned();
        let records = tokio::task::spawn_blocking({
            let account = account.clone();
            move || download_records::load(&database, P::ID, &account)
        })
        .await
        .map_err(io::Error::other)??;
        let mut snapshot = self.status.write().await;
        snapshot.history = Self::history(&account, records);
        snapshot.account = account;
        Ok(())
    }

    pub(super) async fn reconcile(
        &self,
        mut files: Vec<RemoteFile>,
        preserve: bool,
    ) -> io::Result<()> {
        files.sort_by(|a, b| a.size.cmp(&b.size).then_with(|| a.path.cmp(&b.path)));
        let mut paths = HashSet::with_capacity(files.len());
        if files.iter().any(|file| {
            file.id.is_empty() || !self.session.contains(&file.path) || !paths.insert(&file.path)
        }) {
            return Err(io::Error::other("Invalid or duplicate cloud file identity"));
        }
        let account = self.session.account().to_owned();
        let database = self.database.to_owned();
        let (dispositions, mut records) = tokio::task::spawn_blocking({
            let account = account.clone();
            move || -> io::Result<_> {
                Ok((
                    download_queue::load(&database, P::ID, &account)?,
                    download_records::load(&database, P::ID, &account)?,
                ))
            }
        })
        .await
        .map_err(io::Error::other)??;
        let old: HashMap<_, _> = self
            .status
            .read()
            .await
            .files
            .iter()
            .map(|file| (file.id.clone(), file.clone()))
            .collect();
        let seen: HashSet<_> = files
            .iter()
            .map(|file| {
                (
                    file.id.as_str(),
                    file.path.as_str(),
                    self.session.listed_size(file.size),
                )
            })
            .collect();
        let seen_ids: HashSet<_> = files.iter().map(|file| file.id.as_str()).collect();
        let mut completed_files = HashSet::new();
        let mut downloaded_paths = HashSet::new();
        let mut pending_sizes = HashMap::new();
        let mut changes = Records::new();
        for ((file_id, path, source_size), record) in &records {
            let listed_size = self.session.listed_size(*source_size);
            let key = (file_id.as_str(), path.as_str(), listed_size);
            if let Some(completion) = record.completion {
                completed_files.insert(key);
                if matches!(
                    completion.reason,
                    CompletionReason::Downloaded | CompletionReason::AlreadyLocal
                ) {
                    downloaded_paths.insert((path.as_str(), listed_size));
                }
                continue;
            }
            if seen.contains(&key) {
                pending_sizes.insert(key, *source_size);
                continue;
            }
            if !self.session.contains(path)
                || dispositions.get(&(file_id.clone(), path.clone())) == Some(&Disposition::Removed)
            {
                continue;
            }
            let prior = old.get(&task_id(P::ID, &account, file_id, path, *source_size));
            let reason = if paths.contains(path) {
                CompletionReason::RemoteReplaced
            } else if seen_ids.contains(file_id.as_str()) {
                CompletionReason::RemoteMoved
            } else {
                CompletionReason::RemoteDeleted
            };
            changes.insert(
                (file_id.clone(), path.clone(), *source_size),
                Record {
                    total: record.total,
                    downloaded: prior
                        .map_or(record.downloaded, saved_bytes)
                        .min(record.total),
                    completion: Some(Completion {
                        reason,
                        at: auth::now()?,
                    }),
                },
            );
        }

        files.retain(|file| {
            let key = (file.id.clone(), file.path.clone());
            let listed_size = self.session.listed_size(file.size);
            dispositions.get(&key) != Some(&Disposition::Removed)
                && !completed_files.contains(&(file.id.as_str(), file.path.as_str(), listed_size))
                && !downloaded_paths.contains(&(file.path.as_str(), listed_size))
        });
        let files = if files.is_empty() {
            files
        } else {
            self.session.read_local_progress(files).await?
        };

        let mut next = Vec::with_capacity(files.len());
        for file in files {
            let listed_size = self.session.listed_size(file.size);
            let source_size = pending_sizes
                .get(&(file.id.as_str(), file.path.as_str(), listed_size))
                .copied()
                .unwrap_or(file.size);
            let id = task_id(P::ID, &account, &file.id, &file.path, source_size);
            let prior = old.get(&id);
            let disposition = dispositions.get(&(file.id.clone(), file.path.clone()));
            let key = (file.id.clone(), file.path.clone(), source_size);
            let record = records.get(&key);
            let downloaded = prior.map_or_else(
                || record.map_or(0, |record| record.downloaded),
                |row| row.progress.downloaded,
            );
            let progress = match (disposition, prior) {
                (Some(Disposition::Paused), _) => TransferProgress {
                    status: DownloadState::Paused,
                    speed: 0,
                    total: Some(file.size),
                    downloaded,
                },
                (_, Some(row)) if preserve => row.progress,
                _ => TransferProgress {
                    status: DownloadState::Queued,
                    downloaded,
                    total: Some(file.size),
                    speed: 0,
                },
            };
            let total = progress.total.unwrap_or(file.size);
            let pending = Record {
                total,
                downloaded: progress.downloaded.min(total),
                completion: None,
            };
            if record != Some(&pending) {
                changes.insert(key, pending);
            }
            next.push(Download {
                id,
                provider: P::ID,
                path: file.path,
                remote_id: file.id,
                source_size,
                progress,
                last_failure: prior.and_then(|row| row.last_failure),
                local_progress: file.local_progress,
                completion: None,
            });
        }
        self.save_records(&changes).await?;
        records.extend(changes);
        let mut snapshot = self.status.write().await;
        snapshot.account = account.clone();
        snapshot.files = next;
        snapshot.history = Self::history(&account, records);
        Ok(())
    }

    fn history(account: &str, records: Records) -> Vec<Download> {
        records
            .into_iter()
            .filter_map(|((id, path, size), record)| {
                record
                    .completion
                    .map(|_| history_file(P::ID, account, id, path, size, record))
            })
            .collect()
    }

    async fn save_records(&self, records: &Records) -> io::Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let account = self.session.account().to_owned();
        let database = self.database.to_owned();
        let records = records.clone();
        tokio::task::spawn_blocking(move || {
            download_records::save(&database, P::ID, &account, &records)
        })
        .await
        .map_err(io::Error::other)?
    }

    pub(super) async fn archive(
        &self,
        file: &Download,
        progress: TransferProgress,
        reason: CompletionReason,
    ) -> io::Result<()> {
        let total = progress
            .total
            .or(file.progress.total)
            .unwrap_or(progress.downloaded);
        let record = Record {
            total,
            downloaded: match reason {
                CompletionReason::Downloaded | CompletionReason::AlreadyLocal => total,
                CompletionReason::RemoteDeleted
                | CompletionReason::RemoteMoved
                | CompletionReason::RemoteReplaced => saved_bytes(file).min(total),
            },
            completion: Some(Completion {
                reason,
                at: auth::now()?,
            }),
        };
        self.save_records(&Records::from([(
            (file.remote_id.clone(), file.path.clone(), file.source_size),
            record,
        )]))
        .await?;
        let mut snapshot = self.status.write().await;
        snapshot.files.retain(|row| row.id != file.id);
        snapshot.history.push(history_file(
            P::ID,
            self.session.account(),
            file.remote_id.clone(),
            file.path.clone(),
            file.source_size,
            record,
        ));
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
            return self
                .archive(&file, file.progress, CompletionReason::RemoteDeleted)
                .await
                .map_err(|error| {
                    eprintln!(
                        "{} could not save the recycled download record: {error}",
                        P::ID
                    );
                    QueueError::Unavailable
                });
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
