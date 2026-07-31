#!/bin/sh
# Start MinIO and ensure the media bucket exists.
#
# deploy/compose/compose.yml does this with two containers — `minio` plus a
# one-shot `minio-init` that runs `mc mb` once the server is healthy. Railway
# has no depends_on/one-shot equivalent, so both roles collapse into this
# entrypoint: start the server, wait for it to answer, create the bucket, then
# hand the foreground back to the server process.
set -eu

: "${MINIO_ROOT_USER:?MINIO_ROOT_USER must be set}"
: "${MINIO_ROOT_PASSWORD:?MINIO_ROOT_PASSWORD must be set}"
BUCKET="${BUZZ_S3_BUCKET:-buzz-media}"

minio server /data --console-address ":9001" &
minio_pid=$!

# Wait for the health endpoint rather than sleeping a fixed interval.
i=0
until mc alias set local "http://127.0.0.1:9000" \
        "${MINIO_ROOT_USER}" "${MINIO_ROOT_PASSWORD}" >/dev/null 2>&1; do
    i=$((i + 1))
    if [ "$i" -ge 60 ]; then
        echo "minio-init: server did not become ready in 60s" >&2
        kill "$minio_pid" 2>/dev/null || true
        exit 1
    fi
    # If the server died, fail now instead of looping to the timeout.
    kill -0 "$minio_pid" 2>/dev/null || { echo "minio-init: server exited" >&2; exit 1; }
    sleep 1
done

mc mb --ignore-existing "local/${BUCKET}"
# Private by default: the relay signs its own reads, nothing is public.
mc anonymous set none "local/${BUCKET}"
echo "minio-init: bucket '${BUCKET}' ready"

wait "$minio_pid"
