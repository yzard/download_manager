#!/bin/sh
set -eu

cd "$(dirname "$0")"
image=ghcr.io/yzard/download-manager:4.0.2
source_label=org.opencontainers.image.source=https://github.com/yzard/BaiduPCS-go-srv

case "$#:${1:-}" in
    0:) exec docker buildx build --load --label "$source_label" --tag "$image" . ;;
    1:--publish)
        exec docker buildx build --platform linux/amd64,linux/arm64 \
            --label "$source_label" --tag "$image" --push .
        ;;
    *) echo "Usage: $0 [--publish]" >&2; exit 2 ;;
esac
