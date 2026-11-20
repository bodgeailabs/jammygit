#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  printf 'Usage: %s BINARY\n' "$0" >&2
  exit 2
fi

# A musl build can link successfully while still depending on a system loader.
# Check both the ELF interpreter and shared libraries; ldd alone misses this case.
headers=$(readelf --program-headers --wide -- "$1")
dynamic=$(readelf --dynamic --wide -- "$1")
if [[ "$headers" == *INTERP* || "$dynamic" == *'(NEEDED)'* ]]; then
  printf 'Expected a fully static Linux binary, but %s needs a dynamic loader or shared library.\n' "$1" >&2
  exit 1
fi
printf 'Verified static Linux binary: %s\n' "$1"
