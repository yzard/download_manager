use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::http::{HeaderMap, HeaderValue, header};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io,
    time::{Duration, Instant},
};

const SESSION_SECONDS: u64 = 8 * 60 * 60;
const COOKIE: &str = "download_manager_session";

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
    pub username: String,
    pub password_hash: String,
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

#[derive(Default)]
pub struct Sessions {
    active: HashMap<String, Instant>,
    next_login: Option<Instant>,
}

impl Sessions {
    pub fn allow_login_attempt(&mut self) -> bool {
        let now = Instant::now();
        if self.next_login.is_some_and(|next| now < next) {
            return false;
        }
        self.next_login = Some(now + Duration::from_secs(1));
        true
    }

    pub fn create(&mut self) -> io::Result<HeaderValue> {
        let now = Instant::now();
        self.active.retain(|_, expires| *expires > now);
        if self.active.len() >= 32
            && let Some(oldest) = self
                .active
                .iter()
                .min_by_key(|(_, expiry)| *expiry)
                .map(|(token, _)| token.clone())
        {
            self.active.remove(&oldest);
        }
        let mut random = [0; 32];
        getrandom::fill(&mut random).map_err(io::Error::other)?;
        let token = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let cookie = make_cookie(&token, SESSION_SECONDS);
        self.active
            .insert(token, now + Duration::from_secs(SESSION_SECONDS));
        Ok(cookie)
    }

    pub fn contains(&self, token: &str) -> bool {
        self.active
            .get(token)
            .is_some_and(|expires| *expires > Instant::now())
    }

    pub fn revoke(&mut self, token: &str) {
        self.active.remove(token);
    }

    pub fn clear(&mut self) {
        self.active.clear();
        self.next_login = None;
    }
}

pub fn session_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|cookie| cookie.trim().split_once('='))
        .find_map(|(name, value)| {
            (name == COOKIE && value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()))
                .then_some(value)
        })
}

fn make_cookie(value: &str, age: u64) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{COOKIE}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={age}"
    ))
    .expect("the cookie uses a hexadecimal value and numeric age")
}

pub fn expired_cookie() -> HeaderValue {
    make_cookie("", 0)
}
