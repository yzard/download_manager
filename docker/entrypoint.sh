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

case "${1:-}" in
    serve) exec /usr/local/bin/download_manager ;;
    import-baidu|setup-code) exec /usr/local/bin/download_manager "$@" ;;
    *) exec /usr/local/bin/BaiduPCS-Go "$@" ;;
esac
