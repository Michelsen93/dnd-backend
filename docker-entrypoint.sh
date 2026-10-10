#!/bin/sh
# Starts the API. With LITESTREAM_REPLICA_URL set (e.g. gcs://bucket/dnd.sqlite), the database is
# restored from the replica on boot and every change is streamed back while the server runs.
set -e

DB_PATH="${DB_PATH:-/data/dnd.sqlite}"
export DATABASE_URL="sqlite://${DB_PATH}?mode=rwc"

if [ -n "${LITESTREAM_REPLICA_URL}" ]; then
  litestream restore -if-db-not-exists -if-replica-exists -o "${DB_PATH}" "${LITESTREAM_REPLICA_URL}"
  exec litestream replicate -exec /app/backend "${DB_PATH}" "${LITESTREAM_REPLICA_URL}"
fi

echo "LITESTREAM_REPLICA_URL is not set: data lives only inside this container." >&2
exec /app/backend
