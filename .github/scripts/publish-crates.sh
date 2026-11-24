#!/usr/bin/env bash
set -euo pipefail

# A previous attempt may have uploaded the macro crate before the upload of wheels failed.
version=$1
packages=()
for crate in wheels-macros wheels; do
  status=$(curl --silent --show-error --retry 3 --output /dev/null --write-out '%{http_code}' \
    --user-agent 'magicblock-wheels (https://github.com/magicblock-labs/magicblock-wheels)' \
    "https://crates.io/api/v1/crates/$crate/$version")
  case "$status" in
    200) echo "$crate $version is already published" ;;
    404) packages+=(-p "$crate") ;;
    *) echo "Could not check $crate $version on crates.io (HTTP $status)" >&2; exit 1 ;;
  esac
done
if (( ${#packages[@]} )); then
  cargo publish --locked "${packages[@]}"
fi
