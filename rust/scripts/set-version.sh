#!/bin/sh
# Sets the version of the Rust crates: the workspace version in Cargo.toml,
# the version zero-conformance requires of zero-client, and the workspace
# crates' entries in Cargo.lock and in the apps' lockfiles, so
# `cargo --locked` agrees.
#
#   rust/scripts/set-version.sh 0.1.0
set -eu
version="$1"
dir="$(cd "$(dirname "$0")/.." && pwd)"
case "$version" in
  *[!0-9A-Za-z.+-]* | "") echo "bad version: $version" >&2; exit 1 ;;
esac
sed -i.bak -E \
  -e "/^\[workspace\.package\]/,/^\[/ s/^version = \"[^\"]*\"/version = \"$version\"/" \
  -e "s/^(zero-client = \{ path = \"zero-client\", version = )\"[^\"]*\"/\1\"$version\"/" \
  "$dir/Cargo.toml"
rm "$dir/Cargo.toml.bak"
# The apps in this repository (apps/*) are workspaces of their own that
# depend on zero-client by path, so their lockfiles name its version too.
for lock in "$dir/Cargo.lock" "$dir"/../apps/*/Cargo.lock; do
  [ -f "$lock" ] || continue
  sed -i.bak -E \
    -e "/^name = \"zero-(client|conformance)\"$/{n;s/^version = \"[^\"]*\"/version = \"$version\"/;}" \
    "$lock"
  rm "$lock.bak"
done
