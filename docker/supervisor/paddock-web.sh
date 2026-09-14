#!/bin/bash
#
# Supervisor wrapper for paddock's web interface.
#
# paddock web needs the config onstart.sh writes -- the FreeToken venv path, the workspace
# library roots, the loopback [server] and [web] binds -- to exist before it starts.
# Priority ordering in the two supervisor .conf files already starts freetoken-setup first,
# but supervisor does not wait for a lower-priority program to exit before starting the
# next one, so the ordering alone does not guarantee the config is there yet. This waits
# for it instead of racing it.
set -euo pipefail

CFG="${PADDOCK_CONFIG_DIR:-/workspace/config/paddock}/config.toml"
for _ in $(seq 1 60); do
  [ -f "$CFG" ] && break
  sleep 1
done

exec /usr/local/bin/paddock web
