mod auth;
mod settings;
mod storage;
mod web_access;
mod worker;

use auth::{ACCESS_SECONDS, CookieKind, SESSION_SECONDS, TokenHash};
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, FromRequestParts, State},
    http::{HeaderMap, StatusCode, header, request::Parts},
    middleware,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use settings::{
    BaiduSettings, DEFAULT_CHECK_INTERVAL_SECONDS, GeneralSettings, Language, Settings,
};
use std::{
    env,
    io::{self, Read},
    net::SocketAddr,
    path::Path,
    sync::Arc,
};
use storage::accounts;
use tokio::{
    net::TcpListener,
    signal::unix::{SignalKind, signal},
    sync::{Mutex, RwLock, watch},
};
use web_access::RequestSecurity;
use worker::{Download, Snapshot};

const DATABASE: &str = "/data/download_manager.sqlite3";

#[derive(Clone)]
struct AppState {
    settings: watch::Sender<Settings>,
    account_access: Arc<RwLock<auth::AccountAccess>>,
    save_lock: Arc<Mutex<()>>,
    status: Arc<RwLock<Snapshot>>,
}

#[derive(Serialize)]
struct Status {
    provider: &'static str,
    has_tokens: bool,
    setup_required: bool,
    monitor_directory: String,
    check_interval_seconds: u32,
    phase: &'static str,
    files: Vec<Download>,
}

async fn status(State(state): State<AppState>, _session: Session, _body: Bytes) -> Json<Status> {
    let settings = state.settings.borrow().baidu.clone();
    let snapshot = state.status.read().await;
    Json(Status {
        provider: "baidu",
        has_tokens: settings.has_tokens(),
        setup_required: !settings.ready(),
        monitor_directory: settings.monitor_directory.clone(),
        check_interval_seconds: settings.check_interval_seconds,
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
    check_interval_seconds: Option<u32>,
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

#[derive(Clone, Copy)]
struct Session {
    hash: TokenHash,
    secure: bool,
}

impl Session {
    fn from_headers(headers: &HeaderMap, secure: bool) -> Option<Self> {
        Some(Self {
            hash: CookieKind::Access.read(headers, secure)?,
            secure,
        })
    }

    async fn remaining(self) -> Result<Option<i64>, AppError> {
        tokio::task::spawn_blocking(move || {
            let now = auth::now()?;
            accounts::active_access(Path::new(DATABASE), &self.hash, self.secure, now)
                .map(|expiry| expiry.map(|expiry| expiry - now))
        })
        .await
        .map_err(session_error)?
        .map_err(session_error)
    }

    async fn require_active(self) -> Result<(), AppError> {
        self.remaining()
            .await?
            .ok_or(AppError(StatusCode::UNAUTHORIZED, "loginRequired"))?;
        Ok(())
    }
}

impl FromRequestParts<AppState> for Session {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &AppState) -> Result<Self, AppError> {
        let secure = parts
            .extensions
            .get::<RequestSecurity>()
            .ok_or(AppError(StatusCode::FORBIDDEN, "requestFailed"))?
            .0;
        let session = Self::from_headers(&parts.headers, secure)
            .ok_or(AppError(StatusCode::UNAUTHORIZED, "loginRequired"))?;
        session.require_active().await?;
        Ok(session)
    }
}

fn session_error(error: impl std::fmt::Display) -> AppError {
    eprintln!("Download Manager session operation failed: {error}");
    AppError(StatusCode::INTERNAL_SERVER_ERROR, "sessionCreationFailed")
}

#[derive(Serialize)]
struct SessionInfo {
    language: Language,
    refresh_in: Option<i64>,
    can_refresh: bool,
    general: Option<GeneralSettings>,
    account_setup: Option<auth::AccountSetup>,
    username: Option<String>,
}

async fn session_info(
    State(state): State<AppState>,
    Extension(RequestSecurity(secure)): Extension<RequestSecurity>,
    headers: HeaderMap,
    _body: Bytes,
) -> Result<Json<SessionInfo>, AppError> {
    let refresh_in = match Session::from_headers(&headers, secure) {
        Some(session) => session.remaining().await?,
        None => None,
    };
    let access = state.account_access.read().await;
    let account_setup = access.setup_mode();
    let authenticated = account_setup.is_none() && refresh_in.is_some() && access.account.is_some();
    let can_refresh =
        account_setup.is_none() && CookieKind::Refresh.read(&headers, secure).is_some();
    let general = state.settings.borrow().general.clone();
    Ok(Json(SessionInfo {
        language: general.language,
        general: authenticated.then_some(general),
        refresh_in: if authenticated { refresh_in } else { None },
        can_refresh,
        account_setup,
        username: access
            .account
            .as_ref()
            .filter(|_| authenticated)
            .map(|account| account.username.clone()),
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credentials {
    username: String,
    password: String,
}

fn verify_attempt(
    password: &str,
    account: &auth::Account,
    matches_username: bool,
    invalid: AppError,
) -> Result<(), AppError> {
    let path = Path::new(DATABASE);
    let now = auth::now().map_err(session_error)?;
    let throttle = accounts::login_throttle(path).map_err(session_error)?;
    if throttle.retry_after > now {
        return Err(AppError(StatusCode::TOO_MANY_REQUESTS, "retryLogin"));
    }
    let password_matches = auth::verify_password(password, &account.password_hash);
    if !password_matches || !matches_username {
        throttle
            .record_failure(path, auth::now().map_err(session_error)?)
            .map_err(session_error)?;
        return Err(invalid);
    }
    Ok(())
}

async fn login(
    State(state): State<AppState>,
    Extension(RequestSecurity(secure)): Extension<RequestSecurity>,
    Json(input): Json<Credentials>,
) -> Result<(StatusCode, HeaderMap), AppError> {
    let guard = state.save_lock.clone().lock_owned().await;
    let account = {
        let access = state.account_access.read().await;
        if access.setup_mode().is_some() {
            return Err(AppError(StatusCode::CONFLICT, "accountSetupRequired"));
        }
        access
            .account
            .clone()
            .ok_or(AppError(StatusCode::CONFLICT, "accountSetupRequired"))?
    };
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        verify_attempt(
            &input.password,
            &account,
            input.username == account.username,
            AppError(StatusCode::UNAUTHORIZED, "invalidCredentials"),
        )?;
        let (refresh, refresh_hash) = generate_token()?;
        let (access, access_hash) = generate_token()?;
        let path = Path::new(DATABASE);
        accounts::clear_throttle(path).map_err(session_error)?;
        accounts::create_session(
            path,
            &refresh_hash,
            &access_hash,
            secure,
            auth::now().map_err(session_error)?,
            &account.password_hash,
        )
        .map_err(session_error)?;
        let mut headers = HeaderMap::new();
        headers.append(
            header::SET_COOKIE,
            CookieKind::Access.header(&access, ACCESS_SECONDS, secure),
        );
        headers.append(
            header::SET_COOKIE,
            CookieKind::Refresh.header(&refresh, SESSION_SECONDS, secure),
        );
        Ok((StatusCode::NO_CONTENT, headers))
    })
    .await
    .map_err(session_error)?
}

fn generate_token() -> Result<(String, TokenHash), AppError> {
    let token = auth::new_token().map_err(session_error)?;
    let hash = auth::token_hash(&token).expect("generated tokens have 64 hexadecimal digits");
    Ok((token, hash))
}

async fn renew_session(
    Extension(RequestSecurity(secure)): Extension<RequestSecurity>,
    headers: HeaderMap,
    _body: Bytes,
) -> Result<impl IntoResponse, AppError> {
    let refresh = CookieKind::Refresh
        .read(&headers, secure)
        .ok_or(AppError(StatusCode::UNAUTHORIZED, "loginRequired"))?;
    let cookie = tokio::task::spawn_blocking(move || {
        let (access, access_hash) = generate_token()?;
        let now = auth::now().map_err(session_error)?;
        let expires =
            accounts::renew_access(Path::new(DATABASE), &refresh, &access_hash, secure, now)
                .map_err(session_error)?
                .ok_or(AppError(StatusCode::UNAUTHORIZED, "loginRequired"))?;
        Ok::<_, AppError>(CookieKind::Access.header(&access, expires - now, secure))
    })
    .await
    .map_err(session_error)??;
    Ok((StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie)]))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupCredentials {
    username: String,
    password: String,
    setup_code: String,
}

async fn setup_account(
    State(state): State<AppState>,
    Json(input): Json<SetupCredentials>,
) -> Result<StatusCode, AppError> {
    let guard = state.save_lock.clone().lock_owned().await;
    let closed = || AppError(StatusCode::CONFLICT, "accountSetupClosed");
    let reset_pending = {
        let access = state.account_access.read().await;
        if access.setup_mode().is_none() {
            return Err(closed());
        }
        access.reset_pending
    };
    if !auth::valid_username(&input.username) {
        return Err(AppError(StatusCode::BAD_REQUEST, "invalidUsername"));
    }
    if !auth::valid_password(&input.password) {
        return Err(AppError(StatusCode::BAD_REQUEST, "invalidPassword"));
    }

    let invalid_code = || AppError(StatusCode::FORBIDDEN, "invalidSetupCode");
    let code = auth::token_hash(input.setup_code.trim()).ok_or_else(invalid_code)?;
    // Publish the saved account even if the browser disconnects.
    tokio::spawn(async move {
        let _guard = guard;
        let account = tokio::task::spawn_blocking(move || {
            let path = Path::new(DATABASE);
            if !accounts::setup_code_valid(path, &code, auth::now().map_err(save_error)?)
                .map_err(save_error)?
            {
                return Err(invalid_code());
            }
            let account = auth::Account {
                username: input.username,
                password_hash: auth::hash_password(&input.password).map_err(save_error)?,
            };
            if !accounts::save_account_setup(
                path,
                &account,
                reset_pending,
                &code,
                auth::now().map_err(save_error)?,
            )
            .map_err(save_error)?
            {
                return Err(invalid_code());
            }
            Ok(account)
        })
        .await
        .map_err(save_error)??;
        *state.account_access.write().await = auth::AccountAccess {
            account: Some(account),
            reset_pending: false,
        };
        Ok::<_, AppError>(())
    })
    .await
    .map_err(save_error)??;

    Ok(StatusCode::NO_CONTENT)
}

async fn logout(
    Extension(RequestSecurity(secure)): Extension<RequestSecurity>,
    headers: HeaderMap,
    _body: Bytes,
) -> Result<(StatusCode, HeaderMap), AppError> {
    let refresh = CookieKind::Refresh.read(&headers, secure);
    let access = CookieKind::Access.read(&headers, secure);
    if refresh.is_some() || access.is_some() {
        tokio::task::spawn_blocking(move || {
            accounts::revoke_session(Path::new(DATABASE), refresh, access, secure)
        })
        .await
        .map_err(session_error)?
        .map_err(session_error)?;
    }
    Ok((StatusCode::NO_CONTENT, expired_session(secure)))
}

fn expired_session(secure: bool) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.append(header::SET_COOKIE, CookieKind::Access.header("", 0, secure));
    headers.append(
        header::SET_COOKIE,
        CookieKind::Refresh.header("", 0, secure),
    );
    headers
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordInput {
    current_password: String,
    new_password: String,
}

async fn change_password(
    State(state): State<AppState>,
    Extension(RequestSecurity(secure)): Extension<RequestSecurity>,
    session: Session,
    Json(input): Json<PasswordInput>,
) -> Result<impl IntoResponse, AppError> {
    if !auth::valid_password(&input.new_password) {
        return Err(AppError(StatusCode::BAD_REQUEST, "invalidPassword"));
    }
    let guard = state.save_lock.clone().lock_owned().await;
    session.require_active().await?;
    // Keep the write and session revocation together even if the browser disconnects.
    tokio::spawn(async move {
        let _guard = guard;
        let account = state
            .account_access
            .read()
            .await
            .account
            .clone()
            .ok_or(AppError(StatusCode::UNAUTHORIZED, "loginRequired"))?;
        let saved_account = tokio::task::spawn_blocking(move || {
            verify_attempt(
                &input.current_password,
                &account,
                true,
                AppError(StatusCode::BAD_REQUEST, "invalidCurrentPassword"),
            )?;
            let new_hash = auth::hash_password(&input.new_password).map_err(save_error)?;
            accounts::change_password(Path::new(DATABASE), &account.password_hash, &new_hash)
                .map_err(save_error)?;
            Ok::<_, AppError>(auth::Account {
                password_hash: new_hash,
                ..account
            })
        })
        .await
        .map_err(save_error)??;
        state.account_access.write().await.account = Some(saved_account);
        Ok::<_, AppError>(())
    })
    .await
    .map_err(save_error)??;
    Ok((StatusCode::NO_CONTENT, expired_session(secure)))
}

fn save_error(error: impl std::fmt::Display) -> AppError {
    eprintln!("Cannot save Download Manager configuration: {error}");
    AppError(StatusCode::INTERNAL_SERVER_ERROR, "saveFailed")
}

async fn save_baidu_settings(
    State(state): State<AppState>,
    session: Session,
    Json(input): Json<BaiduSettingsInput>,
) -> Result<StatusCode, AppError> {
    let guard = state.save_lock.clone().lock_owned().await;
    session.require_active().await?;
    let mut settings = state.settings.borrow().baidu.clone();
    if !input.bduss.trim().is_empty() {
        settings.bduss = input.bduss.trim().to_owned();
    }
    if !input.stoken.trim().is_empty() {
        settings.stoken = input.stoken.trim().to_owned();
    }
    settings.monitor_directory = input.monitor_directory.trim().to_owned();
    if let Some(interval) = input.check_interval_seconds {
        settings.check_interval_seconds = interval;
    }
    settings
        .validate()
        .map_err(|error| AppError(StatusCode::BAD_REQUEST, error))?;
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        storage::save_baidu(Path::new(DATABASE), &settings).map_err(save_error)?;
        state
            .settings
            .send_modify(|current| current.baidu = settings);
        Ok::<_, AppError>(())
    })
    .await
    .map_err(save_error)??;
    Ok(StatusCode::NO_CONTENT)
}

async fn save_general_settings(
    State(state): State<AppState>,
    session: Session,
    Json(mut settings): Json<GeneralSettings>,
) -> Result<Json<GeneralSettings>, AppError> {
    let guard = state.save_lock.clone().lock_owned().await;
    session.require_active().await?;
    settings.download_directory = settings.download_directory.trim().to_owned();
    settings
        .validate()
        .map_err(|error| AppError(StatusCode::BAD_REQUEST, error))?;
    let destination_changed =
        state.settings.borrow().general.download_directory != settings.download_directory;
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        if destination_changed {
            settings
                .prepare_destination()
                .map_err(|_| AppError(StatusCode::BAD_REQUEST, "unusableDownloadDirectory"))?;
        }
        storage::save_general(Path::new(DATABASE), &settings).map_err(save_error)?;
        state.settings.send_if_modified(|current| {
            current.general = settings.clone();
            destination_changed
        });
        Ok(Json(settings))
    })
    .await
    .map_err(save_error)?
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
            "/locale.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../frontend/locale.js"),
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
        .route("/api/v1/auth/refresh", post(renew_session))
        .route("/api/v1/auth/setup-account", post(setup_account))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/change-password", post(change_password))
        .route("/api/v1/baidu/save-settings", post(save_baidu_settings))
        .route("/api/v1/settings/save-general", post(save_general_settings))
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
        [command] if command == "import-baidu" => return import_baidu(),
        [command] if command == "setup-code" => return setup_code(),
        _ => {
            return Err(io::Error::other(
                "Usage: download_manager [import-baidu | setup-code]",
            ));
        }
    }
    let initial = Settings {
        general: storage::load_general(Path::new(DATABASE))?,
        baidu: storage::load_baidu(Path::new(DATABASE))?,
    };
    let account_access = accounts::load_account_access(Path::new(DATABASE), reset_requested()?)?;
    let allowed_hosts = match env::var("WEB_ALLOWED_HOSTS") {
        Ok(value) => web_access::AllowedHosts::parse(&value)?,
        Err(env::VarError::NotPresent) => web_access::AllowedHosts::default(),
        Err(error) => return Err(io::Error::other(error)),
    };
    let allowed_hosts = match (env::var("WEB_HTTPS_HOST"), env::var("WEB_TRUSTED_PROXY")) {
        (Ok(host), Ok(proxy)) => allowed_hosts.with_https(
            &host,
            proxy
                .parse()
                .map_err(|_| io::Error::other("WEB_TRUSTED_PROXY must be an IP address"))?,
        )?,
        (Err(env::VarError::NotPresent), Err(env::VarError::NotPresent)) => allowed_hosts,
        _ => {
            return Err(io::Error::other(
                "Set WEB_HTTPS_HOST and WEB_TRUSTED_PROXY together",
            ));
        }
    };
    let listener =
        TcpListener::bind(env::var("WEB_BIND").unwrap_or_else(|_| "127.0.0.1:8080".to_owned()))
            .await?;
    let (settings, updates) = watch::channel(initial);
    let status = Arc::new(RwLock::new(Snapshot::default()));
    let state = AppState {
        settings,
        account_access: Arc::new(RwLock::new(account_access)),
        save_lock: Arc::default(),
        status: status.clone(),
    };
    let mut terminate = signal(SignalKind::terminate())?;
    let worker = tokio::spawn(async move { worker::run(updates, &status).await });
    eprintln!("Download Manager listening on {}", listener.local_addr()?);
    let result = axum::serve(
        listener,
        app(state, allowed_hosts).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        tokio::select! { _ = terminate.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
    })
    .await;
    worker.abort();
    let _ = worker.await;
    result
}

fn reset_requested() -> io::Result<bool> {
    match env::var("RESET_USER_PASSWORD") {
        Ok(value) if value == "true" => Ok(true),
        Ok(value) if value == "false" => Ok(false),
        Err(env::VarError::NotPresent) => Ok(false),
        _ => Err(io::Error::other(
            "RESET_USER_PASSWORD must be true or false",
        )),
    }
}

fn setup_code() -> io::Result<()> {
    let code = auth::new_token()?;
    let hash = auth::token_hash(&code).expect("generated setup codes have 64 hexadecimal digits");
    if !accounts::issue_setup_code(Path::new(DATABASE), &hash, reset_requested()?, auth::now()?)? {
        return Err(io::Error::other("Account setup is closed"));
    }
    println!("{code}");
    Ok(())
}

#[derive(Deserialize)]
struct BaiduImportInput {
    #[serde(flatten)]
    baidu: BaiduSettingsInput,
    download_directory: String,
}

fn import_baidu() -> io::Result<()> {
    let mut input = String::new();
    io::stdin().take(24 * 1024 + 1).read_to_string(&mut input)?;
    if input.len() > 24 * 1024 {
        return Err(io::Error::other("Import input is too large"));
    }
    let input: BaiduImportInput =
        serde_json::from_str(&input).map_err(|_| io::Error::other("Invalid Baidu import input"))?;
    let baidu = BaiduSettings {
        bduss: input.baidu.bduss,
        stoken: input.baidu.stoken,
        monitor_directory: input.baidu.monitor_directory,
        check_interval_seconds: input
            .baidu
            .check_interval_seconds
            .unwrap_or(DEFAULT_CHECK_INTERVAL_SECONDS),
    };
    let general = GeneralSettings {
        download_directory: input.download_directory,
        language: Language::default(),
    };
    baidu.validate().map_err(io::Error::other)?;
    general.validate().map_err(io::Error::other)?;
    if !storage::import_baidu(Path::new(DATABASE), &baidu, &general)? {
        return Err(io::Error::other("Baidu settings are already configured"));
    }
    println!("Baidu settings imported into SQLite.");
    Ok(())
}
