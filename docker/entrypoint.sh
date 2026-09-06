#!/usr/bin/env bash
#
# PID 1 for Entrypoint launch mode and for local `docker run`.
#
# Vast's SSH and Jupyter launch modes discard this and run their own startup, including
# their own sshd. That is why the real work lives in onstart.sh, which those modes can
# call from the instance's on-start field. This image ships no sshd of its own: one that
# competes with Vast's is a known cause of instances you cannot log in to.
set -euo pipefail

/opt/ft/onstart.sh

exec "$@"
