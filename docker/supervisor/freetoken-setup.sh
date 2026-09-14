#!/bin/bash
#
# Supervisor wrapper for the FreeToken/paddock instance setup.
#
# The base image owns the entrypoint and runs supervisor as PID 1, so this is how a derived
# image gets work done at boot without fighting it. The actual setup lives in onstart.sh so
# it stays runnable by hand, and on a plain `docker run` outside Vast.
set -euo pipefail
exec /opt/ft/onstart.sh
