#!/bin/sh
set -eu
umask 077
# The new volume is root-owned. Prepare only the fixed application directory,
# then drop privileges before opening the registry or starting any worker.
if [ "$(id -u)" = 0 ]; then
    test ! -L /data/bog
    mkdir -p /data/bog
    chown 10001:10001 /data/bog
    chmod 700 /data/bog
    exec gosu bog "$@"
fi
exec "$@"
