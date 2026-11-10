use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header, uri::Authority},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};
use std::{
    io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Instant,
};

#[derive(Clone, Default)]
pub struct AllowedHosts {
    http: Arc<[Authority]>,
    https: Option<TrustedHttps>,
}

#[derive(Clone)]
struct TrustedHttps {
    host: Authority,
    proxy: IpAddr,
}

#[derive(Clone, Copy)]
pub struct RequestSecurity(pub bool);

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
            .map(|http| Self { http, https: None })
    }

    pub fn with_https(mut self, host: &str, proxy: IpAddr) -> io::Result<Self> {
        let host = host
            .parse::<Authority>()
            .ok()
            .filter(|host| !host.as_str().contains('@') && !host.host().is_empty())
            .ok_or_else(|| io::Error::other("WEB_HTTPS_HOST must contain an HTTPS authority"))?;
        self.https = Some(TrustedHttps { host, proxy });
        Ok(self)
    }

    fn is_http_host(&self, authority: &Authority) -> bool {
        matches!(authority.host(), "localhost" | "127.0.0.1" | "[::1]")
            || self.http.contains(authority)
    }
}

fn request_host(headers: &HeaderMap) -> Option<Authority> {
    let host = single_header(headers, header::HOST)?;
    (!host.contains('@'))
        .then(|| host.parse::<Authority>().ok())
        .flatten()
}

fn single_header(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    let values = headers.get_all(name);
    let mut values = values.iter();
    let value = values.next()?.to_str().ok()?;
    values.next().is_none().then_some(value)
}

fn same_origin(method: &Method, headers: &HeaderMap, host: &Authority, secure: bool) -> bool {
    let scheme = if secure { "https://" } else { "http://" };
    method.is_safe()
        || single_header(headers, header::ORIGIN).and_then(|value| value.strip_prefix(scheme))
            == Some(host.as_str())
}

pub async fn handle(
    State(allowed_hosts): State<AllowedHosts>,
    mut request: Request,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let start = Instant::now();
    let host = request_host(request.headers());
    let secure = allowed_hosts.https.as_ref().is_some_and(|config| {
        host.as_ref() == Some(&config.host)
            && request
                .extensions()
                .get::<ConnectInfo<SocketAddr>>()
                .is_some_and(|peer| peer.0.ip() == config.proxy)
            && single_header(
                request.headers(),
                header::HeaderName::from_static("x-forwarded-proto"),
            ) == Some("https")
    });
    let mut response = if let Some(host) = host {
        let allowed_http = allowed_hosts.is_http_host(&host);
        if secure || (allowed_hosts.https.is_none() && allowed_http) {
            if same_origin(&method, request.headers(), &host, secure) {
                request.extensions_mut().insert(RequestSecurity(secure));
                next.run(request).await
            } else {
                StatusCode::FORBIDDEN.into_response()
            }
        } else if let Some(config) = allowed_hosts
            .https
            .filter(|_| allowed_http && method.is_safe())
        {
            let location = format!(
                "https://{}{}",
                config.host,
                request
                    .uri()
                    .path_and_query()
                    .map_or("/", |path| path.as_str())
            );
            Redirect::temporary(&location).into_response()
        } else {
            StatusCode::FORBIDDEN.into_response()
        }
    } else {
        StatusCode::FORBIDDEN.into_response()
    };
    if secure {
        response.headers_mut().insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000"),
        );
    }
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
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
