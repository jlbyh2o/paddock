#!/usr/bin/env bash
#
# Build, scan, and optionally push the image.
#
#   DOCKER_NAMESPACE=<your-docker-hub-account> docker/build.sh
#   DOCKER_NAMESPACE=<your-docker-hub-account> docker/build.sh --push
#
# The account name is read from the environment and never written to a file, so nothing
# in this repository names where the image is published.
#
# The scan is not advisory. A push happens only after it exits clean.
set -euo pipefail

cd "$(dirname "$0")/.."

IMAGE_NAME="${IMAGE_NAME:-freetoken-ftman}"
PUSH=0
for arg in "$@"; do
  case "$arg" in
    --push) PUSH=1 ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

# Both checks run before the build, not before the push: a missing login should cost a
# second, not a full rebuild and a multi-minute scan.
if [ "$PUSH" = 1 ]; then
  if [ -z "${DOCKER_NAMESPACE:-}" ]; then
    echo "DOCKER_NAMESPACE must be set to push (your Docker Hub account name)." >&2
    exit 2
  fi
  if ! docker system info 2>/dev/null | grep -q "^ Username:"; then
    echo "Not logged in to Docker Hub. Run: docker login -u ${DOCKER_NAMESPACE}" >&2
    echo "Use a Personal Access Token as the password, not your account password." >&2
    exit 2
  fi
fi

STAGING="${IMAGE_NAME}:building"

echo "==> building $STAGING"
# --provenance=false --sbom=false: buildx otherwise attaches provenance attestations that
# record how and where the image was built, including the build context and local paths.
# Those travel with a pushed image. Nothing about this build needs to be attested.
docker buildx build \
  --provenance=false \
  --sbom=false \
  --file docker/Dockerfile \
  --tag "$STAGING" \
  --load \
  .

# Version the tag from what actually landed in the image rather than from what this script
# believes it built, so a tag can never claim a version the image does not have.
echo "==> reading component versions from the image"
FT_VERSION="$(docker run --rm --entrypoint /opt/freetoken/venv/bin/python "$STAGING" \
  -c 'import freetoken; print(freetoken.version.__version__)' | tr -d '\r\n')"
MAN_VERSION="$(docker run --rm --entrypoint ft-man "$STAGING" --version \
  | awk '{print $NF}' | tr -d '\r\n')"
CUDA_MAJOR="$(docker run --rm --entrypoint /bin/bash "$STAGING" \
  -c 'nvcc --version | sed -n "s/.*release \([0-9]*\).*/\1/p" | head -1' | tr -d '\r\n')"

TAG="ft${FT_VERSION}-man${MAN_VERSION}-cu${CUDA_MAJOR}"
echo "    freetoken $FT_VERSION, ft-man $MAN_VERSION, cuda $CUDA_MAJOR  ->  :$TAG"

LOCAL="${IMAGE_NAME}:${TAG}"
docker tag "$STAGING" "$LOCAL"
docker tag "$STAGING" "${IMAGE_NAME}:latest"
docker rmi "$STAGING" >/dev/null 2>&1 || true

echo "==> scanning"
if ! docker/scan-image.sh "$LOCAL"; then
  echo "Scan failed. The image is built and tagged locally as $LOCAL but will not be pushed." >&2
  echo "Fix what it found, rebuild, and try again." >&2
  exit 1
fi

if [ "$PUSH" != 1 ]; then
  cat <<MSG
Built and scanned clean: $LOCAL

Not pushed (no --push). To publish:
  DOCKER_NAMESPACE=<account> docker/build.sh --push
MSG
  exit 0
fi

REMOTE="${DOCKER_NAMESPACE}/${IMAGE_NAME}"

echo "==> pushing $REMOTE:$TAG and :latest"
docker tag "$LOCAL" "${REMOTE}:${TAG}"
docker tag "$LOCAL" "${REMOTE}:latest"
docker push "${REMOTE}:${TAG}"
docker push "${REMOTE}:latest"

cat <<MSG

Pushed ${REMOTE}:${TAG}

On Vast, rent a host with driver r580+ (CUDA 13), then:
  image          ${REMOTE}:${TAG}
  launch mode    Jupyter notebook + SSH   (Entrypoint gives no sshd, so no way in)
  on-start       entrypoint.sh            (required: this mode replaces the entrypoint)
  PORTAL_CONFIG  append  localhost:18919:1919:/:FreeToken API
  ports          do NOT map 1919 - ft serve has no auth; Caddy publishes it on 18919
  env            HF_TOKEN=<token>   (only for gated or private repos)
MSG
