#!/usr/bin/env bash
set -euo pipefail

# Compile and run a consumer outside the source tree so imports use the tarball.
package_dir=$PWD
consumer=$(mktemp -d)
trap 'rm -rf "$consumer"' EXIT
npm pack --pack-destination "$consumer" >/dev/null
cp test/consumer.mts "$consumer/"
(
  cd "$consumer"
  npm install --no-audit --no-fund ./*.tgz
  "$package_dir/node_modules/.bin/tsc" --strict --target ES2022 --module Node16 consumer.mts
  node consumer.mjs
)
if [[ $# -gt 0 ]]; then
  cp "$consumer"/*.tgz "$1"
fi
