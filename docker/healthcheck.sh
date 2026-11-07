#!/bin/sh
# Docker HEALTHCHECK: succeeds when the server answers GET /api/health on the address it was told to listen on.
set -eu

listen="${APM_LISTEN:-0.0.0.0:8080}"
case "$listen" in
  *:*)
    host="${listen%:*}"
    port="${listen##*:}"
    ;;
  *)
    host=""
    port="$listen"
    ;;
esac

# A wildcard listen address is not a destination: probe the loopback interface instead.
case "$host" in
  "" | "0.0.0.0" | "[::]" | "::" | "*") host="127.0.0.1" ;;
esac

exec curl --fail --silent --show-error --max-time 4 --output /dev/null "http://${host}:${port}/api/health"
