#!/usr/bin/env bash
#
# Prove a built image carries nothing that identifies its builder and no credentials.
#
# Exit 0 means clean. Any other exit means do not push. build.sh treats a non-zero exit
# as fatal, on the reasoning that a false positive costs a minute and a miss is public,
# mirrored and cached forever.
#
# Three passes, because one pattern set cannot serve both halves of the problem:
#
#   1. metadata   - env, labels, entrypoint and the full build history. Small, so it gets
#                   the aggressive patterns; every hit is worth reading.
#   2. whole image - the entire filesystem, streamed. Only unambiguous patterns, because
#                   ~20 GB of third-party wheels contains a great many Jeremys who are
#                   not you, and a hard-fail on those would be a hard-fail on every build.
#   3. authored   - the files this build actually wrote. Small and ours, so the weak
#                   patterns mean something here.
set -uo pipefail

IMAGE="${1:-}"
[ -n "$IMAGE" ] || { echo "usage: $0 <image>" >&2; exit 2; }

RED=$'\033[1;31m'; GRN=$'\033[1;32m'; YEL=$'\033[1;33m'; RST=$'\033[0m'
[ -t 1 ] || { RED=''; GRN=''; YEL=''; RST=''; }
fail_count=0
note() { printf '  %s\n' "$*"; }
bad()  { printf '%sFAIL%s %s\n' "$RED" "$RST" "$*"; fail_count=$((fail_count + 1)); }
ok()   { printf '%s ok %s %s\n' "$GRN" "$RST" "$*"; }

# Identity: strings that cannot legitimately appear anywhere in the image, scanned over every
# byte of it with no exceptions.
#
# These live OUTSIDE the repository, in docker/scan-identity, which is gitignored. Publishing
# a scanner whose source lists your username, real name and mail domain would leak exactly the
# thing the scanner exists to catch -- and would leak it in the one file guaranteed to be read
# by anyone auditing the tool. Copy docker/scan-identity.example and fill it in.
IDENTITY_FILE="${SCAN_IDENTITY_FILE:-$(dirname "$0")/scan-identity}"
if [ -f "$IDENTITY_FILE" ]; then
  # shellcheck disable=SC1090
  . "$IDENTITY_FILE"
fi
if [ -z "${IDENTITY:-}" ] || [ -z "${WEAK:-}" ]; then
  printf '%sFAIL%s no identity patterns configured\n' "$RED" "$RST" >&2
  printf '  Expected IDENTITY and WEAK to be set by %s\n' "$IDENTITY_FILE" >&2
  printf '  Create it from docker/scan-identity.example. Refusing to scan without it, because\n' >&2
  printf '  an empty identity pattern would pass every image silently.\n' >&2
  exit 2
fi
# Credentials: scanned everywhere EXCEPT third-party site-packages. Upstream packages
# legitimately ship credential-shaped constants -- cryptography's PEM parser holds
# "-----BEGIN OPENSSH PRIVATE KEY-----" as a literal, and transformers ships a public
# sandbox CI token -- and hard-failing on those would hard-fail on every build forever.
# The trust boundary is authorship, not pattern strength: this build writes no files into
# third-party packages, so a credential of yours cannot land there. The one package it
# does modify, freetoken, is scanned explicitly below.
CREDENTIAL='\bhf_[A-Za-z0-9]{30,}|\bghp_[A-Za-z0-9]{30,}|\bgithub_pat_[A-Za-z0-9_]{20,}|\bsk-ant-[A-Za-z0-9_-]{20,}|\bAKIA[0-9A-Z]{16}\b|-----BEGIN [A-Z ]*PRIVATE KEY-----'
STRONG="$IDENTITY|$CREDENTIAL"
# WEAK (real signals in files we wrote, noise everywhere else) also comes from that file.
# Paths this build authored. Everything else in the image came from upstream.
AUTHORED='/opt/ft /usr/local/bin/ft-man /etc/profile.d /opt/supervisor-scripts/freetoken-setup.sh /etc/supervisor/conf.d/freetoken-setup.conf /opt/freetoken/venv/lib/python3.12/site-packages/freetoken'

# The base image is not ours. Building on vastai/base-image means inheriting an empty
# /root/.ssh (Vast fills it at boot), git checkouts of vast-cli and nvm, and CPython's stdlib
# test certificates -- all of which trip credential and provenance checks and none of which
# this build introduced. So those two passes are DIFFERENTIAL: the same check runs against the
# base and only new findings fail. An allowlist would have to be re-audited every time Vast
# changes their image; a diff cannot rot.
#
# Identity scanning is never differential. A username or host path is fatal wherever it is.
if [ -z "${BASE_IMAGE:-}" ]; then
  _dockerfile="$(dirname "$0")/Dockerfile"
  [ -f "$_dockerfile" ] && BASE_IMAGE="$(sed -n 's/^ARG VASTAI_IMAGE=//p' "$_dockerfile" | head -1)"
fi
BASE_AVAILABLE=0
if [ -n "${BASE_IMAGE:-}" ] && docker image inspect "$BASE_IMAGE" >/dev/null 2>&1; then
  BASE_AVAILABLE=1
fi

# Run a check inside an image and return a sorted, unique path list.
in_image() { docker run --rm --entrypoint /bin/bash "$1" -c "$2" 2>/dev/null | sort -u; }

# Findings from $IMAGE minus findings from the base. Fails closed: with no base image to
# compare against, everything is reported rather than silently passed.
new_findings() {
  local check="$1" ours theirs
  ours="$(in_image "$IMAGE" "$check")"
  if [ "$BASE_AVAILABLE" = 1 ]; then
    theirs="$(in_image "$BASE_IMAGE" "$check")"
    comm -23 <(printf '%s\n' "$ours") <(printf '%s\n' "$theirs")
  else
    printf '%s\n' "$ours"
  fi
}

printf '\nScanning %s\n\n' "$IMAGE"

# --- 1. metadata -------------------------------------------------------------------
# docker history exposes every build argument and command; docker inspect exposes env and
# labels. Both travel with the image to anyone who pulls it.
meta="$(docker inspect "$IMAGE" 2>/dev/null; docker history --no-trunc "$IMAGE" 2>/dev/null)"
if [ -z "$meta" ]; then
  bad "image not found locally: $IMAGE"
  exit 1
fi
if hits="$(printf '%s' "$meta" | grep -oniE "$STRONG|$WEAK" | sort -u)" && [ -n "$hits" ]; then
  bad "image metadata (env, labels or build history) identifies the builder:"
  printf '%s\n' "$hits" | head -20 | sed 's/^/      /'
else
  ok "metadata carries no identifying strings"
fi

# A token that reached ENV is in the image config for everyone who pulls it.
if env_json="$(docker inspect -f '{{json .Config.Env}}' "$IMAGE")" && \
   printf '%s' "$env_json" | grep -qE '"(HF_TOKEN|HUGGING_FACE_HUB_TOKEN|.*_API_KEY|.*_SECRET|.*PASSWORD)='; then
  bad "a credential-shaped variable is baked into the image environment:"
  printf '%s' "$env_json" | tr ',' '\n' | grep -E '(TOKEN|API_KEY|SECRET|PASSWORD)=' | sed 's/^/      /'
else
  ok "no credential-shaped variables in the image environment"
fi

# --- 2. whole filesystem -----------------------------------------------------------
# Streamed through grep rather than unpacked: no 20 GB of scratch space, one pass.
cid="$(docker create "$IMAGE" true)" || { bad "could not create a container to export"; exit 1; }
trap 'docker rm -f "$cid" >/dev/null 2>&1 || true' EXIT

note "streaming the full filesystem through grep (a few minutes for a ~33 GB image)..."
id_hits="$(docker export "$cid" 2>/dev/null | LC_ALL=C grep -a -o -E -m 25 "$IDENTITY" | sort -u)"
if [ -n "$id_hits" ]; then
  bad "the image filesystem identifies the builder:"
  printf '%s\n' "$id_hits" | sed 's/^/      /'
  note "locating it (this re-reads the image, and is slow)..."
  docker run --rm --entrypoint /bin/bash "$IMAGE" -c \
    "grep -rlIE '$IDENTITY' / 2>/dev/null | head -20" | sed 's/^/      /' || true
else
  ok "no identifying strings anywhere in the filesystem"
fi

# Credentials, everywhere the build could have written. site-packages is excluded for the
# reason given above; freetoken, the one package this build patches, is added back.
CRED_CHECK="
  grep -rlIE '$CREDENTIAL' / \
    --exclude-dir=site-packages --exclude-dir=dist-packages \
    --exclude-dir=proc --exclude-dir=sys --exclude-dir=dev 2>/dev/null
  grep -rlIE '$CREDENTIAL' /opt/freetoken/venv/lib/python3.12/site-packages/freetoken 2>/dev/null
  true"
cred_hits="$(new_findings "$CRED_CHECK" | grep -v '^$' | head -20)"
if [ -n "$cred_hits" ]; then
  bad "credential material outside third-party packages:"
  printf '%s\n' "$cred_hits" | sed 's/^/      /'
else
  ok "no credential material in anything this build wrote"
fi

# --- 3. authored files -------------------------------------------------------------
weak_hits="$(docker run --rm --entrypoint /bin/bash "$IMAGE" -c \
  "grep -rnIiE '$WEAK' $AUTHORED 2>/dev/null | head -20")"
if [ -n "$weak_hits" ]; then
  bad "files written by this build mention the builder:"
  printf '%s\n' "$weak_hits" | sed 's/^/      /'
else
  ok "files written by this build are anonymous"
fi

# --- 4. structural -----------------------------------------------------------------
# Things whose mere presence is the problem, regardless of contents.
STRAY_CHECK='
  for p in /root/.netrc /root/.docker/config.json /root/.gitconfig \
           /root/.cache/huggingface/token /workspace/hf/token /opt/freetoken/.git; do
    [ -e "$p" ] && echo "$p"
  done
  # A non-empty /root/.ssh is a finding; the base ships it empty for Vast to fill at boot.
  find /root/.ssh -mindepth 1 2>/dev/null
  find / -maxdepth 6 -name .git -type d 2>/dev/null
  find / -maxdepth 6 -name authorized_keys 2>/dev/null
  true'
strays="$(new_findings "$STRAY_CHECK" | grep -v '^$' | head -20)"
if [ -n "$strays" ]; then
  bad "credential or provenance files are present in the image:"
  printf '%s\n' "$strays" | sed 's/^/      /'
else
  ok "no stray credential, ssh or git-provenance files"
fi
if [ "$BASE_AVAILABLE" = 1 ]; then
  note "credential and provenance checks were differential against $BASE_IMAGE"
else
  note "no base image available to diff against - those checks ran absolute (fail-closed)"
fi

# --- verdict -----------------------------------------------------------------------
echo
if [ "$fail_count" -eq 0 ]; then
  printf '%sclean%s - safe to push\n\n' "$GRN" "$RST"
  exit 0
fi
printf '%s%d check(s) failed - do NOT push%s\n\n' "$RED" "$fail_count" "$RST"
exit 1
