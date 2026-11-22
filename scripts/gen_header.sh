#!/bin/sh
# Regenerates include/rperfetto_sdk.h from src/ffi.rs.
set -e
cd "$(dirname "$0")/.."
cbindgen --config cbindgen.toml --crate rperfetto-sdk --output include/rperfetto_sdk.h 2>&1 | grep -v "^WARN" || true
