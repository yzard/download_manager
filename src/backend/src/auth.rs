use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::http::{HeaderMap, HeaderValue, header};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    io,
    time::{SystemTime, UNIX_EPOCH},
};

pub const ACCESS_SECONDS: i64 = 30 * 60;
pub const SESSION_SECONDS: i64 = 30 * 24 * 60 * 60;
pub const SETUP_CODE_SECONDS: i64 = 15 * 60;

pub type TokenHash = [u8; 32];

#[derive(Clone)]
pub struct Account {
    pub username: String,
    pub password_hash: String,
}

pub struct AccountAccess {
    pub account: Option<Account>,
    pub reset_pending: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountSetup {
    Create,
    Reset,
}

impl AccountAccess {
    pub fn setup_mode(&self) -> Option<AccountSetup> {
        if self.account.is_none() {
            Some(AccountSetup::Create)
        } else if self.reset_pending {
            Some(AccountSetup::Reset)
        } else {
            None
        }
    }
}

pub fn valid_username(username: &str) -> bool {
    (1..=64).contains(&username.len())
        && username
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

pub fn valid_password(password: &str) -> bool {
    password.len() <= 1024 && password.chars().take(12).count() == 12
}

pub fn hash_password(password: &str) -> io::Result<String> {
    let mut salt = [0; 16];
    getrandom::fill(&mut salt).map_err(io::Error::other)?;
    let salt = SaltString::encode_b64(&salt)
        .map_err(|_| io::Error::other("Cannot encode password salt"))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| io::Error::other("Cannot hash password"))
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    password.len() <= 1024
        && PasswordHash::new(hash).is_ok_and(|parsed| {
            parsed.algorithm.as_str() == "argon2id"
                && Argon2::default()
                    .verify_password(password.as_bytes(), &parsed)
                    .is_ok()
        })
}

pub fn now() -> io::Result<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .map_err(io::Error::other)
}

pub fn new_token() -> io::Result<String> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub fn token_hash(value: &str) -> Option<TokenHash> {
    (value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| Sha256::digest(value.as_bytes()).into())
}

#[derive(Clone, Copy)]
pub enum CookieKind {
    Access,
    Refresh,
}

impl CookieKind {
    fn name(self, secure: bool, base_url: &str) -> Cow<'static, str> {
        let name = match (self, secure, base_url.is_empty()) {
            (Self::Access, true, true) => "__Host-download_manager_access",
            (Self::Access, true, false) => "__Secure-download_manager_access",
            (Self::Refresh, true, _) => "__Secure-download_manager_refresh",
            (Self::Access, false, _) => "download_manager_access",
            (Self::Refresh, false, _) => "download_manager_refresh",
        };
        if base_url.is_empty() {
            Cow::Borrowed(name)
        } else {
            let suffix: String = Sha256::digest(base_url.as_bytes())[..6]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            Cow::Owned(format!("{name}_{suffix}"))
        }
    }

    pub fn read(self, headers: &HeaderMap, secure: bool, base_url: &str) -> Option<TokenHash> {
        let cookie_name = self.name(secure, base_url);
        headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(';'))
            .filter_map(|cookie| cookie.trim().split_once('='))
            .find_map(|(name, value)| (name == cookie_name).then(|| token_hash(value)).flatten())
    }

    pub fn header(self, value: &str, age: i64, secure: bool, base_url: &str) -> HeaderValue {
        let name = self.name(secure, base_url);
        let path = match self {
            Self::Access => "/",
            Self::Refresh => "/api/v1/auth",
        };
        let security = if secure { "; Secure" } else { "" };
        HeaderValue::from_str(&format!(
            "{name}={value}; Path={base_url}{path}; HttpOnly; SameSite=Strict; Max-Age={age}{security}"
        ))
        .expect("cookie values use generated hex tokens and numeric ages")
    }
}

#[cfg(test)]
#[path = "../../../tests/backend/src/auth.rs"]
mod tests;
