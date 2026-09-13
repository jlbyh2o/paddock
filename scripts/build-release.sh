#!/usr/bin/env bash
# Build a release binary that runs on an older glibc than this machine's.
#
# The frontend is built here with the local Node, then the Rust build runs inside the
# same rust:1.98.0-slim-bookworm image the container build uses (glibc 2.36), so the
# binary starts on Debian 12/13, Ubuntu 22.04+ and RHEL 9 regardless of what the host
# runs. build.rs embeds web/dist as it finds it, which is why the frontend goes first.
#
#   scripts/build-release.sh              # -> target/bookworm/release/ft-man
#   scripts/build-release.sh --skip-web   # reuse an existing web/dist
set -euo pipefail

cd "$(dirname "$0")/.."

RUST_IMAGE="${RUST_IMAGE:-rust:1.98.0-slim-bookworm}"
TARGET_DIR="${TARGET_DIR:-target/bookworm}"

if [ "${1:-}" != "--skip-web" ]; then
  echo "== building the frontend"
  (cd web && npm ci --no-audit --no-fund && npm run build)
fi

if [ ! -f web/dist/index.html ]; then
  echo "web/dist/index.html is missing; refusing to build a binary with the placeholder page" >&2
  exit 1
fi

echo "== building ft-man in $RUST_IMAGE"
docker run --rm \
  -v "$PWD:/src" -w /src \
  -e CARGO_TARGET_DIR="/src/$TARGET_DIR" \
  "$RUST_IMAGE" \
  cargo build --release --locked

bin="$TARGET_DIR/release/ft-man"
echo "== $bin"
"$bin" --version
floor="$(objdump -T "$bin" | grep -oE 'GLIBC_[0-9.]+' | sort -uV | tail -1 | sed 's/GLIBC_//')"
echo "highest glibc symbol required: $floor"
ls -lh "$bin"
