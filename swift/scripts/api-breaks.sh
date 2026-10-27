#!/bin/sh
# Fails when the public API breaks compared with the last release, unless the
# break is declared: a pending changeset, or the version being released, bumps
# zero-swift enough for it (a minor below 1.0, a major from 1.0).
#
#   scripts/api-breaks.sh [<baseline tag>]     default: the mirror's newest tag
#
# With API_BREAKS_BASELINE_DIR set, the baseline's API dumps are kept there and
# reused, which saves building the baseline (CI caches it per tag).
#
# The digester (`swift package diagnose-api-breaking-changes`) needs the
# package at the root of a git repository, so this runs it in a scratch
# repository shaped like the SwiftPM mirror (wevm/zero-swift): the released tag,
# with this checkout's swift/ committed on top. Its API dumps also hold
# `package` declarations, which apps can't use, so they're removed from both
# sides before comparing. On Linux the Apple-only API (ZeroUI, most of
# ZeroVirtual) isn't built, so swift-macos.yml runs this too.
set -eu
cd "$(dirname "$0")/.."
swift=$(pwd)
root=$(cd .. && pwd)
mirror=https://github.com/wevm/zero-swift.git
products="Zero ZeroUI ZeroVirtual ZeroTesting"

baseline=${1:-$(git ls-remote --tags --refs "$mirror" | sed 's|.*refs/tags/||' |
  sort -V | tail -1)}

work=$(mktemp -d)
dumps=$(mktemp -d)
trap 'rm -rf "$work" "$dumps"' EXIT

git -C "$work" init -q
git -C "$work" fetch -q --depth 1 "$mirror" "refs/tags/$baseline:refs/tags/$baseline"
git -C "$work" checkout -q "$baseline"
git -C "$work" rm -rq .
# This checkout's swift/, with uncommitted changes, as the release would have it.
git ls-files -z --cached --others --exclude-standard |
  tar --null -cf - -T - | tar -xf - -C "$work"
git -C "$work" add -A
git -C "$work" -c user.name=api-breaks -c user.email=api-breaks@localhost \
  commit -qm current --allow-empty

digester=$(command -v swift-api-digester || xcrun --find swift-api-digester 2>/dev/null ||
  echo "$(dirname "$(readlink -f "$(command -v swift)")")/swift-api-digester")

cd "$work"
# Dumps each side's API, keeps only its public declarations, and compares
# them. Stripping `package` declarations from both sides matters: one that
# was public and became `package` reads as removed.
for side in old:$baseline new:HEAD; do
  name=${side%%:*} rev=${side#*:} dir=$dumps/${side%%:*}
  # SwiftPM skips dumping a revision whose dumps are already in the directory.
  [ "$name" = old ] && dir=${API_BREAKS_BASELINE_DIR:-$dir}
  swift package diagnose-api-breaking-changes "$rev" --products $products \
    --baseline-dir "$dir" >"$dumps/$name.log" 2>&1 || true
  if ! ls "$dir"/*/*.json >/dev/null 2>&1; then
    echo "Couldn't dump the API at $rev:" >&2
    cat "$dumps/$name.log" >&2
    exit 1
  fi
  for dump in "$dir"/*/*.json; do
    jq 'walk(if type == "object" and has("children")
      then .children |= map(select(.isInternal != true)) else . end)' \
      "$dump" >"$dumps/$name-$(basename "$dump")"
  done
done
breaks=
for old in "$dumps"/old-*.json; do
  module=$(basename "$old" .json) module=${module#old-}
  new="$dumps/new-$module.json"
  if [ ! -e "$new" ]; then
    breaks="$breaks$module: module has been removed
"
    continue
  fi
  "$digester" -diagnose-sdk -print-module -input-paths "$old" -input-paths "$new" \
    -o "$dumps/$module.txt"
  breaks="$breaks$(grep -v -e '^/\*' -e '^$' "$dumps/$module.txt" || true)
"
done
breaks=$(printf '%s' "$breaks" | sed '/^$/d')
if [ -z "$breaks" ]; then
  echo "No public API breaks since $baseline."
  exit 0
fi

echo "Public API breaks since $baseline:"
printf '%s\n' "$breaks" | sed 's/^/  /'
case $baseline in
  0.*) needed=minor levels='minor|major' ;;
  *) needed=major levels=major ;;
esac
# A pending changeset that declares the break.
if grep -lqE "^['\"]zero-swift['\"]: *($levels)\$" "$root"/.changeset/*.md 2>/dev/null; then
  echo "Allowed: a pending changeset bumps zero-swift by a $needed or more."
  exit 0
fi
# The version pull request, or main after it merged: the changesets are gone,
# and the version being released declares the break.
version=$(cat "$swift/VERSION")
old_major=${baseline%%.*} old_minor=${baseline#*.} old_minor=${old_minor%%.*}
new_major=${version%%.*} new_minor=${version#*.} new_minor=${new_minor%%.*}
if [ "$new_major" -gt "$old_major" ] ||
  { [ "$needed" = minor ] && [ "$new_major" -eq "$old_major" ] &&
    [ "$new_minor" -gt "$old_minor" ]; }; then
  echo "Allowed: $version is a $needed or more after $baseline."
  exit 0
fi
echo "These break apps that build against zero-swift $baseline. Keep the old" \
  "API (deprecate it), or add a changeset that bumps zero-swift by a $needed" \
  "and tells apps what to change." >&2
exit 1
