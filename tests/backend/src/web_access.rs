use super::*;

#[test]
fn allows_only_configured_hosts_and_same_origin_writes() {
    let allowed = AllowedHosts::parse("192.168.1.10:8085, nas.local:8085").unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("192.168.1.10:8085"));
    assert!(allowed.permits(&Method::GET, &headers));
    assert!(!AllowedHosts::default().permits(&Method::GET, &headers));
    assert!(!allowed.permits(&Method::POST, &headers));
    headers.insert(
        header::ORIGIN,
        HeaderValue::from_static("http://other.local:8085"),
    );
    assert!(!allowed.permits(&Method::POST, &headers));
    headers.insert(
        header::ORIGIN,
        HeaderValue::from_static("http://192.168.1.10:8085"),
    );
    assert!(allowed.permits(&Method::POST, &headers));
    headers.insert(header::HOST, HeaderValue::from_static("192.168.1.10:80"));
    assert!(!allowed.permits(&Method::GET, &headers));
    headers.insert(header::HOST, HeaderValue::from_static("localhost:8085"));
    assert!(allowed.permits(&Method::GET, &headers));
    assert!(AllowedHosts::parse("http://nas.local:8085").is_err());
    assert!(AllowedHosts::parse("nas.local").is_err());
}
