use super::*;

#[test]
fn validates_hosts_and_matching_origin() {
    let allowed = AllowedHosts::parse("192.168.1.10:8085, nas.local:8085").unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("192.168.1.10:8085"));
    let host = request_host(&headers).unwrap();
    assert!(allowed.is_http_host(&host));
    assert!(!AllowedHosts::default().is_http_host(&host));
    assert!(same_origin(&Method::GET, &headers, &host, false));
    assert!(!same_origin(&Method::POST, &headers, &host, false));
    headers.insert(
        header::ORIGIN,
        HeaderValue::from_static("http://other.local:8085"),
    );
    assert!(!same_origin(&Method::POST, &headers, &host, false));
    headers.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://192.168.1.10:8085"),
    );
    assert!(same_origin(&Method::POST, &headers, &host, true));
    assert!(!same_origin(&Method::POST, &headers, &host, false));
    headers.insert(
        header::ORIGIN,
        HeaderValue::from_static("http://192.168.1.10:8085"),
    );
    assert!(same_origin(&Method::POST, &headers, &host, false));
    headers.append(
        header::ORIGIN,
        HeaderValue::from_static("http://other.local:8085"),
    );
    assert!(!same_origin(&Method::POST, &headers, &host, false));
    assert!(AllowedHosts::parse("http://nas.local:8085").is_err());
    assert!(AllowedHosts::parse("nas.local").is_err());
}
