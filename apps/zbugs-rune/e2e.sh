#!/usr/bin/env bash
# zbugs-rune against zbugs' backend, end to end: the app runs headless on
# the wall clock (`--realtime`), opens an open issue, comments on it, reacts
# to it, labels it, closes it, edits the comment, creates a label for it,
# makes it internal, and unsubscribes from it, pressing its controls as a
# person would. Opening it marks it viewed. Then Postgres must have each
# change, and the app's last frame must show the edited comment.
#
# It needs the backend running (apps/zbugs-swift/server/server.ts), its
# crew token in ZBUGS_TOKEN, `psql` on the PATH, and ZBUGS_DB_URL for the
# backend's database, such as postgresql://postgres@127.0.0.1:5432/zbugs_swift.
# Run it from apps/zbugs-rune.
set -euo pipefail

: "${ZBUGS_TOKEN:?the backend's crew token}"
: "${ZBUGS_DB_URL:?the backend's database URL}"
sql() { psql "$ZBUGS_DB_URL" -v ON_ERROR_STOP=1 -qAtc "$1"; }

out=$(mktemp -d)
# A short one without comments, so its controls all fit in the window: taps
# land on what shows.
issue=$(sql "select id from issue i where open and not exists
  (select 1 from comment c where c.\"issueID\" = i.id)
  order by length(description), \"shortID\" limit 1")
# The label picker lists the issue's project's labels by name, bytewise.
first_label=$(sql "select l.name from label l join issue i on i.\"projectID\" = l.\"projectID\"
  where i.id = '$issue' order by l.name collate \"C\" limit 1")
sql "delete from \"issueLabel\" where \"issueID\" = '$issue'"
sql "delete from emoji where \"subjectID\" = '$issue'"
sql "update issue set visibility = 'public' where id = '$issue'"
me=$(node -e 'console.log(JSON.parse(Buffer.from(process.argv[1].split(".")[1], "base64url")).sub)' "$ZBUGS_TOKEN")
sql "delete from \"viewState\" where \"issueID\" = '$issue' and \"userID\" = '$me'"
body="e2e-$(date +%s)"
new_label="zz-$body"

# The app opens at the selected issue and has its rows within 12 seconds,
# with time to spare on a slow runner. Typing takes a frame or two a
# character, hence the short comment and the wait before sending it.
mkdir -p "$out/app"
printf '{"selection":"%s"}' "$issue" > "$out/app/settings.json"
cat > "$out/script.json" <<EOF
{"events": [
  {"t": 12.0, "type": "tap", "key": "comment-field"},
  {"t": 12.2, "type": "type", "text": "$body"},
  {"t": 14.0, "type": "tap", "key": "comment-button"},
  {"t": 15.0, "type": "tap", "key": "add-reaction-$issue-trigger"},
  {"t": 15.3, "type": "type", "text": "rocket"},
  {"t": 15.8, "type": "tap", "key": "emoji-🚀"},
  {"t": 16.5, "type": "tap", "key": "labels-trigger"},
  {"t": 17.0, "type": "tap", "key": "label-$first_label"},
  {"t": 17.5, "type": "key", "name": "Escape"},
  {"t": 18.0, "type": "tap", "key": "toggle-open"},
  {"t": 19.0, "type": "tap", "key": "edit-comment"},
  {"t": 19.3, "type": "type", "text": " edited"},
  {"t": 20.5, "type": "tap", "key": "save-comment-edit"},
  {"t": 21.5, "type": "tap", "key": "labels-trigger"},
  {"t": 22.0, "type": "type", "text": "$new_label"},
  {"t": 23.5, "type": "key", "name": "Enter"},
  {"t": 24.0, "type": "key", "name": "Escape"},
  {"t": 25.0, "type": "tap", "key": "visibility-trigger"},
  {"t": 25.5, "type": "tap", "key": "option1"},
  {"t": 26.5, "type": "tap", "key": "notifications-trigger"},
  {"t": 27.0, "type": "tap", "key": "option1"}
]}
EOF

ZBUGS_RUNE_DIR="$out/app" cargo run --locked --quiet -- --realtime \
  --script "$out/script.json" --at 29 --size 1180x760 \
  --tree "$out/tree.json" --snap "${ZBUGS_E2E_SNAP:-$out/e2e.png}"

fail=0
check() {
  if [ "$2" = "$3" ]; then echo "ok: $1"; else echo "FAILED: $1: got '$2', want '$3'"; fail=1; fi
}
check "the comment, edited" \
  "$(sql "select count(*) from comment where \"issueID\" = '$issue' and body = '$body edited'")" 1
check "the reaction" \
  "$(sql "select string_agg(value, ',') from emoji where \"subjectID\" = '$issue'")" "🚀"
check "the labels" "$(sql "select string_agg(l.name, ',' order by l.name collate \"C\")
  from \"issueLabel\" il join label l on l.id = il.\"labelID\" where il.\"issueID\" = '$issue'")" \
  "$first_label,$new_label"
check "closed" "$(sql "select open from issue where id = '$issue'")" f
check "internal" "$(sql "select visibility from issue where id = '$issue'")" internal
# Commenting subscribed the user; then they unsubscribed.
check "unsubscribed" "$(sql "select subscribed from \"issueNotifications\"
  where \"issueID\" = '$issue' and \"userID\" = '$me'")" f
check "viewed" "$(sql "select v.viewed >= i.modified from \"viewState\" v
  join issue i on i.id = v.\"issueID\" where i.id = '$issue' and v.\"userID\" = '$me'")" t
check "the comment on screen" "$(grep -q "$body edited" "$out/tree.json" && echo yes)" yes
exit $fail
