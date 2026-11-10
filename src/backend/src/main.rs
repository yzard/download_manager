mod auth;
mod settings;
mod storage;
mod web_access;
mod worker;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, FromRequestParts, State},
    http::{HeaderMap, StatusCode, header, request::Parts},
    middleware,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use settings::BaiduSettings;
use std::{
    env,
    io::{self, Read},
    path::Path,
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::TcpListener,
    signal::unix::{SignalKind, signal},
    sync::{Mutex, RwLock, watch},
};
use worker::{Download, Snapshot};

const DATABASE: &str = "/data/download_manager.sqlite3";

#[derive(Clone)]
struct AppState {
    baidu_settings: watch::Sender<BaiduSettings>,
    account: Arc<RwLock<Option<auth::Account>>>,
    save_lock: Arc<Mutex<()>>,
    status: Arc<RwLock<Snapshot>>,
    sessions: Arc<Mutex<auth::Sessions>>,
}

#[derive(Serialize)]
struct Status {
    provider: &'static str,
    has_tokens: bool,
    setup_required: bool,
    monitor_directory: String,
    download_directory: String,
    phase: &'static str,
    files: Vec<Download>,
}

async fn status(State(state): State<AppState>, _session: Session, _body: Bytes) -> Json<Status> {
    let settings = state.baidu_settings.borrow().clone();
    let snapshot = state.status.read().await;
    Json(Status {
        provider: "baidu",
        has_tokens: settings.has_tokens(),
        setup_required: !settings.ready(),
        monitor_directory: settings.monitor_directory.clone(),
        download_directory: settings.download_directory.clone(),
        phase: if settings.ready() {
            snapshot.phase
        } else {
            "setup"
        },
        files: snapshot.files(),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BaiduSettingsInput {
    #[serde(default)]
    bduss: String,
    #[serde(default)]
    stoken: String,
    monitor_directory: String,
    download_directory: String,
}

struct AppError(StatusCode, &'static str);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        #[derive(Serialize)]
        struct ErrorBody {
            error: &'static str,
        }
        (self.0, Json(ErrorBody { error: self.1 })).into_response()
    }
}

struct Session(String);

impl Session {
    async fn require_active(&self, state: &AppState) -> Result<(), AppError> {
        if state.sessions.lock().await.contains(&self.0) {
            Ok(())
        } else {
            Err(AppError(StatusCode::UNAUTHORIZED, "请先登录。"))
        }
    }
}

impl FromRequestParts<AppState> for Session {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        let token = auth::session_token(&parts.headers)
            .ok_or(AppError(StatusCode::UNAUTHORIZED, "请先登录。"))?;
        let session = Session(token.to_owned());
        session.require_active(state).await?;
        Ok(session)
    }
}

#[derive(Serialize)]
struct SessionInfo {
    account_configured: bool,
    username: Option<String>,
}

async fn session_info(
    State(state): State<AppState>,
    headers: HeaderMap,
    _body: Bytes,
) -> Json<SessionInfo> {
    let account = state.account.read().await.clone();
    let authenticated = if let Some(token) = auth::session_token(&headers) {
        state.sessions.lock().await.contains(token)
    } else {
        false
    };
    Json(SessionInfo {
        account_configured: account.is_some(),
        username: account
            .filter(|_| authenticated)
            .map(|account| account.username),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginInput {
    username: String,
    password: String,
}

async fn login(
    State(state): State<AppState>,
    Json(input): Json<LoginInput>,
) -> Result<impl IntoResponse, AppError> {
    if !state.sessions.lock().await.allow_login_attempt() {
        return Err(AppError(StatusCode::TOO_MANY_REQUESTS, "请稍等一秒再试。"));
    }
    let _guard = state.save_lock.lock().await;
    let account = state.account.read().await.clone().ok_or(AppError(
        StatusCode::CONFLICT,
        "请先在服务器上初始化允许登录的账号。",
    ))?;
    let verified = tokio::task::spawn_blocking(move || {
        let password_matches = auth::verify_password(&input.password, &account.password_hash);
        password_matches && input.username == account.username
    })
    .await
    .map_err(|error| {
        eprintln!("Password verification failed: {error}");
        AppError(StatusCode::INTERNAL_SERVER_ERROR, "无法验证密码。")
    })?;
    if !verified {
        return Err(AppError(StatusCode::UNAUTHORIZED, "用户名或密码错误。"));
    }
    let cookie = state
        .sessions
        .lock()
        .await
        .create()
        .map_err(|_| AppError(StatusCode::INTERNAL_SERVER_ERROR, "无法建立登录会话。"))?;
    Ok((StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie)]))
}

async fn logout(
    State(state): State<AppState>,
    session: Session,
    _body: Bytes,
) -> impl IntoResponse {
    state.sessions.lock().await.revoke(&session.0);
    (
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, auth::expired_cookie())],
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordInput {
    current_password: String,
    new_password: String,
}

async fn change_password(
    State(state): State<AppState>,
    session: Session,
    Json(input): Json<PasswordInput>,
) -> Result<impl IntoResponse, AppError> {
    if !auth::valid_password(&input.new_password) {
        return Err(AppError(
            StatusCode::BAD_REQUEST,
            "新密码至少需要 12 个字符，且最多 1024 字节。",
        ));
    }
    let guard = state.save_lock.clone().lock_owned().await;
    session.require_active(&state).await?;
    // Keep the write and session revocation together even if the browser disconnects.
    tokio::spawn(async move {
        let _guard = guard;
        let account = state
            .account
            .read()
            .await
            .clone()
            .ok_or(AppError(StatusCode::UNAUTHORIZED, "请先登录。"))?;
        let saved_account = tokio::task::spawn_blocking(move || {
            if !auth::verify_password(&input.current_password, &account.password_hash) {
                return Err(AppError(StatusCode::BAD_REQUEST, "当前密码错误。"));
            }
            let new_hash = auth::hash_password(&input.new_password).map_err(save_error)?;
            storage::change_password(Path::new(DATABASE), &account.password_hash, &new_hash)
                .map_err(save_error)?;
            Ok::<_, AppError>(auth::Account {
                password_hash: new_hash,
                ..account
            })
        })
        .await
        .map_err(save_error)??;
        let mut sessions = state.sessions.lock().await;
        *state.account.write().await = Some(saved_account);
        sessions.clear();
        Ok::<_, AppError>(())
    })
    .await
    .map_err(save_error)??;
    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, auth::expired_cookie())],
    ))
}

fn save_error(error: impl std::fmt::Display) -> AppError {
    eprintln!("Cannot save Download Manager configuration: {error}");
    AppError(
        StatusCode::INTERNAL_SERVER_ERROR,
        "无法保存设置，请检查配置目录权限。",
    )
}

async fn save_settings(
    State(state): State<AppState>,
    session: Session,
    Json(input): Json<BaiduSettingsInput>,
) -> Result<StatusCode, AppError> {
    let guard = state.save_lock.clone().lock_owned().await;
    session.require_active(&state).await?;
    let mut settings = state.baidu_settings.borrow().clone();
    if !input.bduss.trim().is_empty() {
        settings.bduss = input.bduss.trim().to_owned();
    }
    if !input.stoken.trim().is_empty() {
        settings.stoken = input.stoken.trim().to_owned();
    }
    settings.monitor_directory = input.monitor_directory.trim().to_owned();
    settings.download_directory = input.download_directory.trim().to_owned();
    settings
        .validate()
        .map_err(|error| AppError(StatusCode::BAD_REQUEST, error))?;
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        settings.prepare_destination().map_err(|_| {
            AppError(
                StatusCode::BAD_REQUEST,
                "无法使用这个保存目录，请检查它是否为真实目录，以及容器是否有写入权限。",
            )
        })?;
        storage::save_baidu(Path::new(DATABASE), &settings, true).map_err(save_error)?;
        state.baidu_settings.send_replace(settings);
        Ok::<_, AppError>(())
    })
    .await
    .map_err(save_error)??;
    Ok(StatusCode::NO_CONTENT)
}

fn app(state: AppState, web_access: web_access::AllowedHosts) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("../../frontend/index.html")) }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../frontend/app.js"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../../frontend/style.css"),
                )
            }),
        )
        .route("/api/v1/baidu/status", post(status))
        .route("/api/v1/auth/session", post(session_info))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/change-password", post(change_password))
        .route("/api/v1/baidu/save-settings", post(save_settings))
        .layer(DefaultBodyLimit::max(24 * 1024))
        .layer(middleware::from_fn_with_state(
            web_access,
            web_access::handle,
        ))
        .with_state(state)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> io::Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    match args.as_slice() {
        [] => {}
        [command, username] if command == "init-user" => return init_user(username),
        [command] if command == "import-baidu" => return import_baidu(),
        _ => {
            return Err(io::Error::other(
                "Usage: download_manager [init-user USERNAME]",
            ));
        }
    }
    let initial = storage::load_baidu(Path::new(DATABASE))?;
    let account = storage::load_account(Path::new(DATABASE))?;
    let interval = env::var("CHECK_INTERVAL_SECONDS")
        .unwrap_or_else(|_| "300".to_owned())
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| io::Error::other("CHECK_INTERVAL_SECONDS must be a positive integer"))?;
    let allowed_hosts = match env::var("WEB_ALLOWED_HOSTS") {
        Ok(value) => web_access::AllowedHosts::parse(&value)?,
        Err(env::VarError::NotPresent) => web_access::AllowedHosts::default(),
        Err(error) => return Err(io::Error::other(error)),
    };
    let listener =
        TcpListener::bind(env::var("WEB_BIND").unwrap_or_else(|_| "127.0.0.1:8080".to_owned()))
            .await?;
    let (settings, updates) = watch::channel(initial);
    let status = Arc::new(RwLock::new(Snapshot::default()));
    let state = AppState {
        baidu_settings: settings,
        account: Arc::new(RwLock::new(account)),
        save_lock: Arc::default(),
        status: status.clone(),
        sessions: Arc::default(),
    };
    let mut terminate = signal(SignalKind::terminate())?;
    let worker =
        tokio::spawn(
            async move { worker::run(updates, &status, Duration::from_secs(interval)).await },
        );
    eprintln!("Download Manager listening on {}", listener.local_addr()?);
    let result = axum::serve(listener, app(state, allowed_hosts))
        .with_graceful_shutdown(async move {
            tokio::select! { _ = terminate.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
        })
        .await;
    worker.abort();
    let _ = worker.await;
    result
}

fn init_user(username: &str) -> io::Result<()> {
    if !auth::valid_username(username) {
        return Err(io::Error::other(
            "Username must be 1–64 letters, digits, dots, underscores or hyphens",
        ));
    }
    let path = Path::new(DATABASE);
    if storage::load_account(path)?.is_some() {
        return Err(io::Error::other(
            "The allowed account is already configured",
        ));
    }
    let password = rpassword::prompt_password("Password (at least 12 characters): ")?;
    if !auth::valid_password(&password) {
        return Err(io::Error::other(
            "Password must be at least 12 characters and at most 1024 bytes",
        ));
    }
    if password != rpassword::prompt_password("Confirm password: ")? {
        return Err(io::Error::other("Passwords did not match"));
    }
    let account = auth::Account {
        username: username.to_owned(),
        password_hash: auth::hash_password(&password)?,
    };
    if !storage::create_account(path, &account)? {
        return Err(io::Error::other(
            "The allowed account is already configured",
        ));
    }
    println!("Allowed account configured. Restart Download Manager to load it.");
    Ok(())
}

fn import_baidu() -> io::Result<()> {
    let mut input = String::new();
    io::stdin().take(24 * 1024 + 1).read_to_string(&mut input)?;
    if input.len() > 24 * 1024 {
        return Err(io::Error::other("Import input is too large"));
    }
    let input: BaiduSettingsInput =
        serde_json::from_str(&input).map_err(|_| io::Error::other("Invalid Baidu import input"))?;
    let settings = BaiduSettings {
        bduss: input.bduss,
        stoken: input.stoken,
        monitor_directory: input.monitor_directory,
        download_directory: input.download_directory,
    };
    settings.validate().map_err(io::Error::other)?;
    if !storage::save_baidu(Path::new(DATABASE), &settings, false)? {
        return Err(io::Error::other("Baidu settings are already configured"));
    }
    println!("Baidu settings imported into SQLite.");
    Ok(())
}
