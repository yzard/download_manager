mod auth;
mod settings;
mod worker;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, FromRequestParts, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header, request::Parts},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};
use settings::Settings;
use std::{
    env,
    fs::OpenOptions,
    io,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    net::TcpListener,
    signal::unix::{SignalKind, signal},
    sync::{Mutex, RwLock, watch},
};
use worker::{Download, Snapshot};

const CONFIG: &str = "/config/config.toml";

#[derive(Clone)]
struct AppState {
    settings: watch::Sender<Settings>,
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
    phase: &'static str,
    files: Vec<Download>,
}

async fn status(State(state): State<AppState>, _session: Session) -> Json<Status> {
    let settings = state.settings.borrow().clone();
    let snapshot = state.status.read().await;
    Json(Status {
        provider: "baidu",
        has_tokens: settings.has_tokens(),
        setup_required: !settings.ready(),
        monitor_directory: settings.monitor_directory.clone(),
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
struct SettingsInput {
    #[serde(default)]
    bduss: String,
    #[serde(default)]
    stoken: String,
    monitor_directory: String,
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

async fn session_info(State(state): State<AppState>, headers: HeaderMap) -> Json<SessionInfo> {
    let account = state.settings.borrow().account.clone();
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
    let account = state.settings.borrow().account.clone().ok_or(AppError(
        StatusCode::CONFLICT,
        "请先在服务器上初始化允许登录的账号。",
    ))?;
    let verified = tokio::task::spawn_blocking(move || {
        let password_matches = auth::verify_password(&input.password, &account.password_hash);
        password_matches && input.username == account.username
    })
    .await
    .unwrap_or(false);
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
        let mut settings = state.settings.borrow().clone();
        let account = settings
            .account
            .as_mut()
            .ok_or(AppError(StatusCode::UNAUTHORIZED, "请先登录。"))?;
        let old_hash = account.password_hash.clone();
        let new_hash = tokio::task::spawn_blocking(move || {
            if !auth::verify_password(&input.current_password, &old_hash) {
                return Err(AppError(StatusCode::BAD_REQUEST, "当前密码错误。"));
            }
            auth::hash_password(&input.new_password).map_err(|_| save_error())
        })
        .await
        .map_err(|_| save_error())??;
        account.password_hash = new_hash;
        let saved = tokio::task::spawn_blocking(move || {
            settings.save(Path::new(CONFIG))?;
            Ok::<_, io::Error>(settings)
        })
        .await
        .map_err(|_| save_error())?
        .map_err(|_| save_error())?;
        let mut sessions = state.sessions.lock().await;
        // Password changes update the account without waking the download worker.
        state.settings.send_if_modified(|settings| {
            settings.account = saved.account;
            false
        });
        sessions.clear();
        Ok::<_, AppError>(())
    })
    .await
    .map_err(|_| save_error())??;
    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, auth::expired_cookie())],
    ))
}

fn save_error() -> AppError {
    AppError(
        StatusCode::INTERNAL_SERVER_ERROR,
        "无法保存设置，请检查配置目录权限。",
    )
}

async fn save_settings(
    State(state): State<AppState>,
    session: Session,
    Json(input): Json<SettingsInput>,
) -> Result<StatusCode, AppError> {
    let guard = state.save_lock.clone().lock_owned().await;
    session.require_active(&state).await?;
    let mut settings = state.settings.borrow().clone();
    if !input.bduss.trim().is_empty() {
        settings.bduss = input.bduss.trim().to_owned();
    }
    if !input.stoken.trim().is_empty() {
        settings.stoken = input.stoken.trim().to_owned();
    }
    settings.monitor_directory = input.monitor_directory.trim().to_owned();
    settings
        .validate()
        .map_err(|error| AppError(StatusCode::BAD_REQUEST, error))?;
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        settings.save(Path::new(CONFIG))?;
        state.settings.send_replace(settings);
        Ok::<_, io::Error>(())
    })
    .await
    .ok()
    .and_then(Result::ok)
    .ok_or_else(save_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn local_requests(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let start = Instant::now();
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    let local_host = host
        .and_then(|value| value.parse::<axum::http::uri::Authority>().ok())
        .is_some_and(|value| matches!(value.host(), "localhost" | "127.0.0.1" | "[::1]"));
    let same_origin = host.is_some_and(|host| {
        request
            .headers()
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
            .and_then(|origin| origin.strip_prefix("http://"))
            == Some(host)
    });
    let mut response =
        if !local_host || (matches!(method, Method::PUT | Method::POST) && !same_origin) {
            StatusCode::FORBIDDEN.into_response()
        } else {
            next.run(request).await
        };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // Polling status requests are omitted to keep download logs readable.
    if !matches!(path.as_str(), "/api/status" | "/api/session") || !response.status().is_success() {
        eprintln!(
            "{method} {path:?} {} {}ms",
            response.status().as_u16(),
            start.elapsed().as_millis()
        );
    }
    response
}

fn app(state: AppState) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("../web/index.html")) }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/app.js"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../web/style.css"),
                )
            }),
        )
        .route("/api/status", get(status))
        .route("/api/session", get(session_info))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/password", put(change_password))
        .route("/api/settings/baidu", put(save_settings))
        .layer(DefaultBodyLimit::max(24 * 1024))
        .layer(middleware::from_fn(local_requests))
        .with_state(state)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> io::Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    match args.as_slice() {
        [] => {}
        [command, username] if command == "init-user" => return init_user(username),
        _ => {
            return Err(io::Error::other(
                "Usage: download-manager [init-user USERNAME]",
            ));
        }
    }
    let initial = Settings::load(Path::new(CONFIG)).map_err(io::Error::other)?;
    let interval = env::var("CHECK_INTERVAL_SECONDS")
        .unwrap_or_else(|_| "300".to_owned())
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| io::Error::other("CHECK_INTERVAL_SECONDS must be a positive integer"))?;
    let listener =
        TcpListener::bind(env::var("WEB_BIND").unwrap_or_else(|_| "127.0.0.1:8080".to_owned()))
            .await?;
    let (settings, updates) = watch::channel(initial);
    let status = Arc::new(RwLock::new(Snapshot::default()));
    let state = AppState {
        settings,
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
    let result = axum::serve(listener, app(state))
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
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open("/config/.account-init.lock")?;
    lock.lock()?;
    let path = Path::new(CONFIG);
    let mut settings = Settings::load(path).map_err(io::Error::other)?;
    if settings.account.is_some() {
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
    settings.account = Some(auth::Account {
        username: username.to_owned(),
        password_hash: auth::hash_password(&password)?,
    });
    settings.save(path)?;
    println!("Allowed account configured. Restart Download Manager to load it.");
    Ok(())
}
