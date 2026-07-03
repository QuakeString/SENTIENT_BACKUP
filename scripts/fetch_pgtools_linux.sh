#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# Licensed under the Apache License, Version 2.0.
#
# Stage pg_dump / pg_restore (+ their shared libraries) for bundling into the
# Linux app, so it doesn't depend on the host's PostgreSQL version. EDB has no
# Linux "binaries" zip, so these come from the PGDG apt packages installed in CI.
# The copied libs get an rpath of $ORIGIN and the tools get $ORIGIN/../lib, so
# the bundled pg_dump finds the bundled libpq etc. regardless of the host.
#
# Usage: scripts/fetch_pgtools_linux.sh [/usr/lib/postgresql/18/bin]
set -euo pipefail

PGBIN="${1:-/usr/lib/postgresql/18/bin}"
HERE="$(cd "$(dirname "$0")" && pwd)"
STAGE="$HERE/../src-tauri/pgtools"

rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$STAGE/lib"

# glibc / core libs that are always present on the host — never bundle these.
EXCLUDE='^(libc|libm|libdl|libpthread|librt|libresolv|ld-linux.*|linux-vdso)\.so'

copy_lib() {
  local so base
  so="$1"
  base="$(basename "$so")"
  [[ "$base" =~ $EXCLUDE ]] && return 0
  [[ -f "$STAGE/lib/$base" ]] && return 0
  cp -L "$so" "$STAGE/lib/$base"
}

for tool in pg_dump pg_restore; do
  cp "$PGBIN/$tool" "$STAGE/bin/$tool"
  chmod +x "$STAGE/bin/$tool"
  # transitive shared-library dependencies (ldd resolves the whole tree)
  while read -r so; do
    copy_lib "$so"
  done < <(ldd "$PGBIN/$tool" | awk '/=> \// {print $3}')
done

# Make the tools find bundled libs, and libs find each other.
patchelf --set-rpath '$ORIGIN/../lib' "$STAGE/bin/pg_dump" "$STAGE/bin/pg_restore"
for lib in "$STAGE"/lib/*.so*; do
  patchelf --set-rpath '$ORIGIN' "$lib" 2>/dev/null || true
done

echo "staged pg tools ($("$STAGE/bin/pg_dump" --version 2>/dev/null || echo '?')):"
ls -1 "$STAGE/bin"
echo "bundled libs: $(ls -1 "$STAGE/lib" | wc -l)"
