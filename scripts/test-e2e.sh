#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
cargo build --workspace
python3 -m unittest discover -s tests/e2e -p 'test_*.py' -v
cargo test --workspace
(
  cd apps/desktop
  if command -v volta >/dev/null 2>&1; then
    volta run --node 24.20.0 --npm 11.19.0 npm run build
    volta run --node 24.20.0 --npm 11.19.0 npm run test:ui
  else
    npm run build
    npm run test:ui
  fi
)
