#!/bin/sh
set -eu

if [ "$#" -ne 0 ]; then
    echo "Usage: $0" >&2
    exit 2
fi

cd "$(dirname "$0")"
./build_docker.sh

if [ ! -e playground/.env ]; then
    {
        printf 'PUID=%s\nPGID=%s\n' "$(id -u)" "$(id -g)"
        cat playground/.env.example
    } > playground/.env
fi
mkdir -p playground/config playground/data/download_manager playground/downloads

compose_file=docker/docker-compose.yaml
compose_config=$(docker compose -f "$compose_file" config --format json)
printf '%s\n' "$compose_config" | python3 -c '
import json
import sys

service = json.load(sys.stdin)["services"]["download_manager"]
port, = service["ports"]
address = port["host_ip"]
published = port["published"]
target = port["target"]
print(f"Port {address}:{published} -> {target}")
print("Open http://127.0.0.1:" + port["published"])
for authority in service["environment"].get("WEB_ALLOWED_HOSTS", "").split(","):
    if authority.strip():
        print("Open http://" + authority.strip())
'

exec docker compose -f "$compose_file" up --no-build --pull never
