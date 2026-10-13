#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 || ( $1 != staple && $1 != validate ) ]]; then
  printf 'Usage: stapler.sh <staple|validate> <App path>\n' >&2
  exit 64
fi

for attempt in 1 2 3 4; do
  if xcrun stapler "$1" "$2"; then
    exit 0
  else
    stapler_status=$?
  fi
  # EX_NOHOST includes unavailable or unexpected ticket-service responses.
  # Invalid ticket data and revoked tickets fail immediately.
  if [[ $stapler_status -ne 68 || $attempt -eq 4 ]]; then
    exit "$stapler_status"
  fi
  retry_delay=$((attempt * 2))
  printf 'stapler returned 68; retrying in %ss (%s/4).\n' "$retry_delay" "$attempt" >&2
  sleep "$retry_delay"
done
