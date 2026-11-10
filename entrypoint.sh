#!/bin/sh
set -eu

case "${UMASK:-077}" in
    [0-7][0-7][0-7]|0[0-7][0-7][0-7]) umask "${UMASK:-077}" ;;
    *) echo "UMASK must contain three octal digits, optionally preceded by zero." >&2; exit 2 ;;
esac

if [ -n "${TZ:-}" ]; then
    case "$TZ" in
        /*|*..*) echo "TZ must be an IANA time zone." >&2; exit 2 ;;
    esac
    if [ ! -f "/usr/share/zoneinfo/$TZ" ]; then
        echo "TZ must be an IANA time zone." >&2
        exit 2
    fi
fi

if [ "$(/bin/busybox id -u)" -eq 0 ]; then
    run_uid=${PUID:-1000}
    run_gid=${PGID:-1000}
    if ! [ "$run_uid" -gt 0 ] 2>/dev/null || ! [ "$run_gid" -ge 0 ] 2>/dev/null; then
        echo "PUID must be a positive UID and PGID must be a valid numeric GID." >&2
        exit 2
    fi
    exec /bin/busybox chpst -u "$run_uid:$run_gid" /bin/busybox sh /entrypoint.sh "$@"
fi

cli=/usr/local/bin/BaiduPCS-Go
login_hash_file=/config/.login-hash

login_from_environment() {
    if [ -z "${BAIDU_BDUSS:-}" ] || [ -z "${BAIDU_STOKEN:-}" ]; then
        echo "Set BAIDU_BDUSS and BAIDU_STOKEN in the container environment." >&2
        return 1
    fi

    fingerprint=$(printf '%s\n%s' "$BAIDU_BDUSS" "$BAIDU_STOKEN" | /bin/busybox sha256sum)
    fingerprint=${fingerprint%% *}
    saved_fingerprint=
    if [ -f "$login_hash_file" ]; then
        IFS= read -r saved_fingerprint < "$login_hash_file" || true
    fi

    if [ "$1" = force ] || [ ! -f /config/pcs_config.json ] || [ "$saved_fingerprint" != "$fingerprint" ]; then
        if ! login_output=$("$cli" login "-bduss=$BAIDU_BDUSS" "-stoken=$BAIDU_STOKEN" 2>&1); then
            echo "Baidu login failed. Check BAIDU_BDUSS and BAIDU_STOKEN." >&2
            return 1
        fi
        case "$login_output" in
            *百度帐号登录成功:*) ;;
            *) echo "Baidu login failed. Check BAIDU_BDUSS and BAIDU_STOKEN." >&2; return 1 ;;
        esac
        (umask 077; printf '%s\n' "$fingerprint" > "$login_hash_file")
    fi
}

case "${1:-}" in
    serve) exec /usr/local/bin/download-manager ;;
    init-user) exec /usr/local/bin/download-manager "$@" ;;
    login-env)
        if [ "$#" -ne 1 ]; then
            echo "Usage: login-env" >&2
            exit 2
        fi
        login_from_environment force
        exec "$cli" who
        ;;
    auto-download)
        if [ "$#" -ne 1 ]; then
            echo "Usage: auto-download" >&2
            exit 2
        fi
        monitor_directory=${MONITOR_DIRECTORY:-}
        case "$monitor_directory" in
            /*) ;;
            *) echo "MONITOR_DIRECTORY must be an absolute Baidu Netdisk path." >&2; exit 2 ;;
        esac
        login_from_environment cached
        exec "$cli" download --saveto /downloads "$monitor_directory"
        ;;
    *) exec "$cli" "$@" ;;
esac
