use super::*;
use std::sync::Mutex;
use tokio::{
    sync::oneshot,
    time::{sleep, timeout},
};

#[derive(Clone)]
struct TestProvider(Arc<Mutex<TestCloud>>);

struct TestCloud {
    files: Vec<(&'static str, u64)>,
    started: Vec<String>,
    stopped: Vec<String>,
    recycled: Vec<String>,
    list_fails: bool,
    recycle_fails: bool,
}

struct TestTransfer {
    cloud: TestProvider,
    path: String,
}

impl DownloadProvider for TestProvider {
    const ID: &'static str = "fixture";
    type Session = Self;
    fn ready(&self, _: &Settings) -> bool {
        true
    }
    fn scan_interval(&self, _: &Settings) -> Duration {
        Duration::from_millis(25)
    }
    fn recycle_on_remove(settings: &Settings) -> bool {
        settings.baidu.delete_on_remove
    }
    async fn connect(&self, _: &Settings) -> io::Result<Self> {
        Ok(self.clone())
    }
}

impl DownloadSession for TestProvider {
    type Transfer = TestTransfer;
    fn account(&self) -> &str {
        "account"
    }
    fn current(&self, _: &Settings) -> bool {
        true
    }
    fn contains(&self, path: &str) -> bool {
        path.starts_with('/')
    }
    async fn list(&self) -> io::Result<Vec<RemoteFile>> {
        let state = self.0.lock().unwrap();
        if state.list_fails {
            return Err(io::Error::other("test list error"));
        }
        Ok(state
            .files
            .iter()
            .map(|(path, size)| RemoteFile {
                id: path.to_string(),
                path: path.to_string(),
                size: *size,
            })
            .collect())
    }
    async fn start(&self, file: &Download) -> io::Result<TestTransfer> {
        self.0.lock().unwrap().started.push(file.path.clone());
        Ok(TestTransfer {
            cloud: self.clone(),
            path: file.path.clone(),
        })
    }
    async fn recycle(&self, file: &Download, _: &watch::Receiver<Settings>) -> io::Result<()> {
        let mut state = self.0.lock().unwrap();
        if state.recycle_fails {
            return Err(io::Error::other("test recycle error"));
        }
        state.recycled.push(file.path.clone());
        state.files.retain(|(path, _)| *path != file.path);
        Ok(())
    }
    async fn after_complete(
        &self,
        _: &TestTransfer,
        _: &watch::Receiver<Settings>,
    ) -> io::Result<bool> {
        Ok(false)
    }
}

impl DownloadTransfer for TestTransfer {
    async fn next(&mut self) -> io::Result<TransferEvent> {
        std::future::pending().await
    }
    async fn stop(&mut self) -> io::Result<()> {
        self.cloud.0.lock().unwrap().stopped.push(self.path.clone());
        Ok(())
    }
}

async fn wait_until(mut condition: impl AsyncFnMut() -> bool) {
    timeout(Duration::from_secs(3), async {
        while !condition().await {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

async fn action(
    sender: &mpsc::Sender<QueueRequest>,
    id: &str,
    action: QueueAction,
    recycle: bool,
) -> Result<(), QueueError> {
    let (reply, receiver) = oneshot::channel();
    assert!(
        sender
            .send(QueueRequest {
                id: id.to_owned(),
                action,
                recycle,
                reply
            })
            .await
            .is_ok()
    );
    timeout(Duration::from_secs(2), receiver)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn one_shared_queue_pauses_resumes_and_reconciles_external_removals() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("test.sqlite3");
    let cloud = TestProvider(Arc::new(Mutex::new(TestCloud {
        files: vec![("/a", 10), ("/b", 20), ("/c", 30)],
        started: Vec::new(),
        stopped: Vec::new(),
        recycled: Vec::new(),
        list_fails: false,
        recycle_fails: false,
    })));
    let status = Arc::new(RwLock::new(Snapshot::default()));
    let (sender, receiver) = mpsc::channel(16);
    let (settings, updates) = watch::channel(Settings {
        baidu: Default::default(),
        general: Default::default(),
    });
    let worker = tokio::spawn({
        let (cloud, status) = (cloud.clone(), status.clone());
        async move {
            run(cloud, updates, &status, receiver, &database).await;
        }
    });
    wait_until(async || cloud.0.lock().unwrap().started == ["/a"]).await;
    let ids: std::collections::HashMap<_, _> = status
        .read()
        .await
        .files()
        .into_iter()
        .map(|file| (file.path, (file.id, file.progress.total)))
        .collect();
    let (a, b, c) = (&ids["/a"].0, &ids["/b"].0, &ids["/c"].0);
    assert_eq!(ids["/b"].1, Some(20));
    assert_eq!(ids["/c"].1, Some(30));

    assert!(action(&sender, b, QueueAction::Pause, false).await.is_ok());
    assert!(cloud.0.lock().unwrap().stopped.is_empty());
    assert!(action(&sender, a, QueueAction::Pause, false).await.is_ok());
    wait_until(async || cloud.0.lock().unwrap().started == ["/a", "/c"]).await;
    assert!(action(&sender, a, QueueAction::Resume, false).await.is_ok());
    assert!(action(&sender, c, QueueAction::Remove, false).await.is_ok());
    wait_until(async || cloud.0.lock().unwrap().started == ["/a", "/c", "/a"]).await;
    assert!(cloud.0.lock().unwrap().recycled.is_empty());

    {
        let mut state = cloud.0.lock().unwrap();
        state.list_fails = true;
        state.files.retain(|(path, _)| *path != "/a");
    }
    sleep(Duration::from_millis(65)).await;
    assert_eq!(status.read().await.files().len(), 2);
    assert_eq!(cloud.0.lock().unwrap().stopped, ["/a", "/c"]);
    cloud.0.lock().unwrap().list_fails = false;
    wait_until(async || {
        status
            .read()
            .await
            .files()
            .iter()
            .map(|file| file.path.as_str())
            .eq(["/b"])
    })
    .await;
    wait_until(async || cloud.0.lock().unwrap().stopped == ["/a", "/c", "/a"]).await;
    assert_eq!(
        status.read().await.files()[0].progress.status,
        DownloadState::Paused
    );

    settings.send_modify(|settings| settings.baidu.delete_on_remove = true);
    assert!(matches!(
        action(&sender, b, QueueAction::Remove, false).await,
        Err(QueueError::Conflict)
    ));
    cloud.0.lock().unwrap().recycle_fails = true;
    assert!(matches!(
        action(&sender, b, QueueAction::Remove, true).await,
        Err(QueueError::RecycleFailed)
    ));
    assert_eq!(
        status.read().await.files()[0].progress.status,
        DownloadState::Paused
    );
    cloud.0.lock().unwrap().recycle_fails = false;
    assert!(action(&sender, b, QueueAction::Remove, true).await.is_ok());
    assert_eq!(cloud.0.lock().unwrap().recycled, ["/b"]);
    assert!(status.read().await.files().is_empty());
    assert_eq!(cloud.0.lock().unwrap().files, [("/c", 30)]);
    worker.abort();
}

#[tokio::test]
async fn retry_keeps_the_last_progress_and_error_until_the_next_attempt() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("retry.sqlite3");
    let provider = TestProvider(Arc::new(Mutex::new(TestCloud {
        files: vec![("/retry", 10)],
        started: Vec::new(),
        stopped: Vec::new(),
        recycled: Vec::new(),
        list_fails: false,
        recycle_fails: false,
    })));
    let (_sender, settings) = watch::channel(Settings {
        baidu: Default::default(),
        general: Default::default(),
    });
    let status = RwLock::new(Snapshot::default());
    let queue = Queue::<TestProvider> {
        session: &provider,
        settings: &settings,
        status: &status,
        interval: Duration::from_secs(60),
        database: &database,
    };
    queue
        .reconcile(provider.list().await.unwrap(), false)
        .await
        .unwrap();
    schedule_scan(&status, queue.interval).await;
    {
        let mut snapshot = status.write().await;
        let row = &mut snapshot.files[0];
        row.progress.status = DownloadState::Failed;
        row.progress.downloaded = 4;
        row.last_failure = Some(FailureReason::TlsHandshakeTimeout);
    }
    queue
        .reconcile(provider.list().await.unwrap(), true)
        .await
        .unwrap();
    {
        let snapshot = status.read().await;
        let row = &snapshot.files[0];
        assert_eq!(row.progress.status, DownloadState::Failed);
        assert_eq!(row.progress.downloaded, 4);
        assert!(
            snapshot.next_check_at().unwrap()
                > SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
        );
    }
    queue
        .reconcile(provider.list().await.unwrap(), false)
        .await
        .unwrap();
    let snapshot = status.read().await;
    let row = &snapshot.files[0];
    assert_eq!(row.progress.status, DownloadState::Queued);
    assert_eq!(row.progress.downloaded, 4);
    assert_eq!(row.progress.total, Some(10));
    assert_eq!(row.last_failure, Some(FailureReason::TlsHandshakeTimeout));
}
