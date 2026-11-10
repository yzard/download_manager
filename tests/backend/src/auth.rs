use super::*;

#[test]
fn scopes_cookies_to_the_mount_and_transport() {
    let token = new_token().unwrap();
    let mut headers = HeaderMap::new();
    let access = CookieKind::Access.header(&token, ACCESS_SECONDS, true, "/tools/download-manager");
    let refresh =
        CookieKind::Refresh.header(&token, SESSION_SECONDS, true, "/tools/download-manager");
    for (header_value, path) in [
        (access, "Path=/tools/download-manager/"),
        (refresh, "Path=/tools/download-manager/api/v1/auth"),
    ] {
        let value = header_value.to_str().unwrap();
        assert!(value.starts_with("__Secure-"));
        assert!(value.contains(path));
        assert!(value.contains("HttpOnly; SameSite=Strict"));
        assert!(value.ends_with("; Secure"));
        headers.append(
            header::COOKIE,
            HeaderValue::from_str(value.split(';').next().unwrap()).unwrap(),
        );
    }
    assert_eq!(
        CookieKind::Access.read(&headers, true, "/tools/download-manager"),
        token_hash(&token)
    );
    assert_eq!(
        CookieKind::Refresh.read(&headers, true, "/tools/download-manager"),
        token_hash(&token)
    );
    assert!(CookieKind::Access.read(&headers, true, "/tools").is_none());
    assert!(
        CookieKind::Access
            .read(&headers, false, "/tools/download-manager")
            .is_none()
    );
}
