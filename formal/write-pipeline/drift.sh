#!/usr/bin/env bash
# Prints one line when code that the write pipeline model covers changed since the sync
# commit, in the working tree too. The skill .claude/skills/write-pipeline/SKILL.md holds
# the sync commit (`sync`) and the covered code (`watch PATH` or `watch PATH fn NAME`).
# Always exits 0, and needs no Java.
set -uo pipefail

cd "$(dirname "$0")/../.."
skill=.claude/skills/write-pipeline/SKILL.md
sync="$(sed -n 's/^sync \([0-9a-f]\{7,40\}\)$/\1/p' "$skill")"

# fn_body NAME: the text of each `fn NAME` in stdin, from its signature to its closing brace.
fn_body() {
  awk -v name="$1" '
    !inside && $0 ~ ("^[ ]*(pub(\\([a-z]+\\))?[ ]+)?(async[ ]+)?fn " name "[(<]") {
      inside = 1; match($0, /^[ ]*/); close_line = sprintf("%" RLENGTH "s}", "")
    }
    inside { print; if ($0 == close_line) inside = 0 }
  '
}

if ! git cat-file -e "$sync^{commit}" 2>/dev/null; then
  echo "write pipeline model: sync commit '$sync' is not in this clone, so drift is unknown"
  exit 0
fi

drifted=()
while read -r path fn name; do
  if [ "$fn" = fn ]; then
    if [ "$(git show "$sync:$path" 2>/dev/null | fn_body "$name")" != "$(fn_body "$name" <"$path")" ]; then
      drifted+=("$path:$name")
    fi
  elif ! git diff --quiet "$sync" -- "$path"; then
    drifted+=("$path")
  fi
done < <(sed -n 's/^watch //p' "$skill")

if [ "${#drifted[@]}" -gt 0 ]; then
  echo "write pipeline model: ${drifted[*]} changed since $sync; follow $skill"
fi
exit 0
