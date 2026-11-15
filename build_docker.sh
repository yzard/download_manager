#!/bin/sh
set -eu

cd "$(dirname "$0")"
repository=ghcr.io/yzard/download_manager
version=$(cat VERSION)
if ! printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
    echo "VERSION must contain a numeric MAJOR.MINOR.PATCH version" >&2
    exit 2
fi
case "$version" in
    *[!0-9.]*)
        echo "VERSION must contain a single version" >&2
        exit 2
        ;;
esac
image="$repository:$version"
latest="$repository:latest"
version_label="org.opencontainers.image.version=$version"
source_label=org.opencontainers.image.source=https://github.com/yzard/download_manager

case "$#:${1:-}" in
    0:)
        exec docker buildx build --file docker/Dockerfile --load \
            --label "$source_label" --label "$version_label" --tag "$image" --tag "$latest" .
        ;;
    1:--image) printf '%s\n' "$image" ;;
    1:--publish)
        exec docker buildx build --file docker/Dockerfile --platform linux/amd64,linux/arm64 \
            --label "$source_label" --label "$version_label" --tag "$image" --tag "$latest" --push .
        ;;
    3:--publish-platform)
        case "$2" in
            linux/amd64|linux/arm64) ;;
            *) echo "Unsupported publish platform: $2" >&2; exit 2 ;;
        esac
        exec docker buildx build --file docker/Dockerfile --platform "$2" --metadata-file "$3" \
            --label "$source_label" --label "$version_label" \
            --output "type=image,name=$repository,push-by-digest=true,name-canonical=true,push=true" .
        ;;
    *) echo "Usage: $0 [--image | --publish | --publish-platform PLATFORM METADATA_FILE]" >&2; exit 2 ;;
esac
