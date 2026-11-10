#!/bin/sh
set -eu

cd "$(dirname "$0")"
image=ghcr.io/yzard/download_manager:4.0.2
source_label=org.opencontainers.image.source=https://github.com/yzard/download_manager

case "$#:${1:-}" in
    0:) exec docker buildx build --load --label "$source_label" --tag "$image" . ;;
    1:--publish)
        exec docker buildx build --platform linux/amd64,linux/arm64 \
            --label "$source_label" --tag "$image" --push .
        ;;
    3:--publish-platform)
        case "$2" in
            linux/amd64|linux/arm64) ;;
            *) echo "Unsupported publish platform: $2" >&2; exit 2 ;;
        esac
        exec docker buildx build --platform "$2" --metadata-file "$3" \
            --label "$source_label" \
            --output "type=image,name=${image%:*},push-by-digest=true,name-canonical=true,push=true" .
        ;;
    *) echo "Usage: $0 [--publish | --publish-platform PLATFORM METADATA_FILE]" >&2; exit 2 ;;
esac
