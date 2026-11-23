#!/usr/bin/env bash
# Records res/demo.gif: the client, the server image, and the scripted model in model.py, in a
# throwaway home with a seeded Atuin history.
#
# Needs cargo, docker, python3, sqlite3, atuin and vhs. IMAGE overrides the server image.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
image=${IMAGE:-ghcr.io/atuinsh/atuin-ai-server:latest}
container=atuin-ai-demo
# The model's port is also in server.toml.
server_port=18080
model_port=18081

cargo build --release --locked --manifest-path "$repo/client/Cargo.toml"

DEMO=$(mktemp -d)
export DEMO
cleanup() {
    docker rm -f "$container" >/dev/null 2>&1 || true
    [[ -z ${model_pid-} ]] || kill "$model_pid" 2>/dev/null || true
    rm -rf "$DEMO"
}
trap cleanup EXIT

project=$DEMO/home/api
mkdir -p "$project/build" "$project/data" "$DEMO/bin"
ln -s "$repo/client/target/release/atuin-ai" "$DEMO/bin/atuin-ai"
cp "$here/.zshrc" "$DEMO/.zshrc"
git -C "$project" init -q

# What the first question finds: two large files from this week, and one from long ago.
sparse_mib() { dd if=/dev/zero of="$1" bs=1 count=0 seek=$(($2 * 1024 * 1024)) 2>/dev/null; }
sparse_mib "$project/data/fixtures.sqlite" 180
sparse_mib "$project/build/api-1.4.2.tar.gz" 64
sparse_mib "$project/data/legacy-dump.sql" 410
touch -t 202501010000 "$project/data/legacy-dump.sql"

# What the second question finds: a week of history, in seconds ago.
now=$(date +%s)
while IFS='|' read -r ago command; do
    printf ': %s:0;%s\n' $((now - ago)) "$command"
done > "$DEMO/zsh_history" <<'EOF'
518400|git clone git@github.com:acme/api.git
432000|npm install
431000|docker build -t api:dev .
345600|git switch -c feat/arm-runners
259200|npm test
172800|docker compose up -d postgres
97200|docker buildx build --platform linux/amd64 -t acme/api:1.4.2 --push .
93600|kubectl rollout status deploy/api -n staging
90000|git push -u origin feat/arm-runners
18000|npm run lint -- --fix
7200|git status
EOF
ATUIN_HOME=$DEMO/atuin HISTFILE=$DEMO/zsh_history atuin import zsh >/dev/null
sqlite3 "$DEMO/atuin/data/history.db" \
    "update history set cwd = '$project', hostname = 'laptop:dev', author = 'dev'"

cat > "$DEMO/atuin/config.toml" <<EOF
[ai]
endpoint = "http://127.0.0.1:$server_port"
session_continue_minutes = 0
tips = false
EOF

python3 "$here/model.py" "$model_port" &
model_pid=$!
docker run -d --rm --name "$container" -p "127.0.0.1:$server_port:8080" \
    --add-host host.docker.internal:host-gateway \
    -v "$here/server.toml:/etc/atuin-ai/config.toml:ro" "$image" >/dev/null
for _ in {1..50}; do
    curl -sf "http://127.0.0.1:$server_port/api/cli/models" >/dev/null && break
    sleep 0.2
done

cd "$repo"
vhs res/demo.tape
