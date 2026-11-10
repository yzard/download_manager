use axum::{
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header, uri::Authority},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{io, sync::Arc, time::Instant};

#[derive(Clone, Default)]
pub struct AllowedHosts(Arc<[Authority]>);

impl AllowedHosts {
    pub fn parse(input: &str) -> io::Result<Self> {
        if input.trim().is_empty() {
            return Ok(Self::default());
        }
        input
            .split(',')
            .map(|host| {
                host.trim()
                    .parse::<Authority>()
                    .ok()
                    .filter(|authority| {
                        !authority.as_str().contains('@')
                            && !authority.host().is_empty()
                            && authority.port_u16().is_some()
                    })
                    .ok_or_else(|| io::Error::other("WEB_ALLOWED_HOSTS requires host:port entries"))
            })
            .collect::<io::Result<Arc<[_]>>>()
            .map(Self)
    }

    fn permits(&self, method: &Method, headers: &HeaderMap) -> bool {
        let Some(host) = headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
        else {
            return false;
        };
        let Ok(authority) = host.parse::<Authority>() else {
            return false;
        };
        let allowed_host = !host.contains('@')
            && (matches!(authority.host(), "localhost" | "127.0.0.1" | "[::1]")
                || self.0.contains(&authority));
        if !allowed_host {
            return false;
        }
        method.is_safe()
            || headers
                .get(header::ORIGIN)
                .and_then(|value| value.to_str().ok())
                .and_then(|origin| origin.strip_prefix("http://"))
                == Some(host)
    }
}

pub async fn handle(
    State(allowed_hosts): State<AllowedHosts>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let start = Instant::now();
    let mut response = if allowed_hosts.permits(&method, request.headers()) {
        next.run(request).await
    } else {
        StatusCode::FORBIDDEN.into_response()
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
    if !matches!(
        path.as_str(),
        "/api/v1/baidu/status" | "/api/v1/auth/session"
    ) || !response.status().is_success()
    {
        eprintln!(
            "{method} {path:?} {} {}ms",
            response.status().as_u16(),
            start.elapsed().as_millis()
        );
    }
    response
}

#[cfg(test)]
#[path = "../../../tests/backend/src/web_access.rs"]
mod tests;
