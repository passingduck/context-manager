#!/bin/bash
# Builds a fixture tree, runs the installed Claude Code against a local capture sink
# with an isolated CLAUDE_CONFIG_DIR, and saves what it sent and which files it loaded.
# Usage: exp.sh <scenario> [extra claude args...]
# Never touches ~/.claude: config, transcripts and logs all land under $EXP.
set -u
R=/private/tmp/claude-501/-Users-sungjin-context-manger/dddccb01-94a9-4428-bd2b-2f11d7667ddd/scratchpad/research
SC=$1; shift
EXP=/private/tmp/claude-501/-Users-sungjin-context-manger/dddccb01-94a9-4428-bd2b-2f11d7667ddd/ccm-exp/$SC
rm -rf "$EXP"; mkdir -p "$EXP"
CFG=$EXP/cfg; TOP=$EXP/top; REPO=$TOP/repo; APP=$REPO/app
mkdir -p "$CFG/rules" "$REPO/docs" "$REPO/.claude/rules/sub" "$APP/.claude/rules" "$APP/nested" "$REPO/src"

w() { mkdir -p "$(dirname "$1")"; printf '%s\n' "$2" > "$1"; }

# user scope (config dir)
w "$CFG/CLAUDE.md" "MARK-USER-CLAUDE"
w "$CFG/rules/u1.md" "MARK-USER-RULE-NOPATHS"
w "$CFG/rules/u2.md" $'---\npaths: "**/*.py"\n---\nMARK-USER-RULE-PY'
cat > "$CFG/settings.json" <<EOF
{"hooks":{"InstructionsLoaded":[{"hooks":[{"type":"command","command":"cat >> '$EXP/il.jsonl'; echo >> '$EXP/il.jsonl'"}]}]}}
EOF

# ancestor above the repo
w "$TOP/CLAUDE.md" "MARK-TOP-ANCESTOR"
w "$TOP/.claude/rules/top.md" "MARK-TOP-RULE"

# repo root
( cd "$REPO" && git init -q . )
cat > "$REPO/CLAUDE.md" <<'EOF'
<!-- MARK-HTML-COMMENT-SHOULD-STRIP -->
MARK-REPO-CLAUDE
Import chain: @docs/i1.md
Code span: `@docs/span.md`
```
@docs/fenced.md
<!-- MARK-COMMENT-IN-FENCE-KEPT -->
```
Inline comment: <!-- @docs/comment.md --> after
Email-like foo@docs/email.md
Space escaped: @docs/with\ space.md
Missing: @docs/missing.md
Binary ext: @docs/pic.png
EOF
w "$REPO/docs/i1.md" $'MARK-I1\n@i2.md'
w "$REPO/docs/i2.md" $'MARK-I2\n@i3.md'
w "$REPO/docs/i3.md" $'MARK-I3\n@i4.md'
w "$REPO/docs/i4.md" $'MARK-I4\n@i5.md'
w "$REPO/docs/i5.md" $'MARK-I5\n@i6.md'
w "$REPO/docs/i6.md" "MARK-I6"
w "$REPO/docs/span.md" "MARK-SPAN-SHOULD-NOT-IMPORT"
w "$REPO/docs/fenced.md" "MARK-FENCED-SHOULD-NOT-IMPORT"
w "$REPO/docs/comment.md" "MARK-COMMENT-IMPORT"
w "$REPO/docs/email.md" "MARK-EMAIL-SHOULD-NOT-IMPORT"
w "$REPO/docs/with space.md" "MARK-SPACE-IMPORT"
w "$REPO/docs/pic.png" "MARK-PNG-SHOULD-NOT-IMPORT"
w "$REPO/.claude/CLAUDE.md" "MARK-REPO-DOTCLAUDE-CLAUDE"
w "$REPO/CLAUDE.local.md" "MARK-REPO-LOCAL"
w "$REPO/AGENTS.md" "MARK-REPO-AGENTS"
w "$REPO/.claude/rules/a.md" "MARK-RULE-A-NOPATHS"
w "$REPO/.claude/rules/sub/b.md" "MARK-RULE-SUB-B-NOPATHS"
w "$REPO/.claude/rules/c.md" $'---\npaths:\n  - "src/**/*.ts"\n---\nMARK-RULE-C-TS'
w "$REPO/.claude/rules/star.md" $'---\npaths: "**"\n---\nMARK-RULE-STAR'
w "$REPO/.claude/rules/notmd.txt" "MARK-RULE-TXT-SHOULD-NOT-LOAD"
w "$REPO/.claude/rules/badyaml.md" $'---\npaths: [unclosed\n---\nMARK-RULE-BADYAML'

# cwd (app) and below
w "$APP/CLAUDE.md" "MARK-APP-CLAUDE"
w "$APP/.claude/rules/d.md" "MARK-APP-RULE-D"
w "$APP/nested/CLAUDE.md" "MARK-NESTED-SHOULD-BE-LAZY"

# auto memory, keyed by the git root of the repo
KEY=$(python3 -I -c 'import re,sys; print(re.sub(r"[^a-zA-Z0-9]","-",sys.argv[1]))' "$REPO")
MEM=$CFG/projects/$KEY/memory
mkdir -p "$MEM"
python3 -I - "$MEM/MEMORY.md" <<'EOF'
import sys
with open(sys.argv[1], "w") as f:
    f.write("@docs/i1.md MARK-MEMORY-IMPORT-TEST\n")
    for i in range(2, 251):
        f.write(f"- MARK-MEM-LINE-{i:03d}\n")
EOF
w "$MEM/topic.md" "MARK-TOPIC-SHOULD-NOT-PRELOAD"

PORT=$(( 20000 + RANDOM % 20000 ))
if [ -n "${PLAN_REL:-}" ]; then
  python3 -I -c 'import json,sys; print(json.dumps([sys.argv[1]+"/"+r for r in sys.argv[2].split()]))' "$EXP" "$PLAN_REL" > "$EXP/plan.json"
  python3 -I "$R/tools/fake_model.py" "$EXP/requests" "$PORT" "$EXP/plan.json" & SRV=$!
else
  python3 -I "$R/tools/capture_server.py" "$EXP/requests" "$PORT" & SRV=$!
fi
sleep 0.5
eval "${PRE:-}"
cd "$EXP/${CWD_REL:-top/repo/app}" || exit 1
env -i HOME="$HOME" PATH="/usr/bin:/bin:/usr/sbin:/sbin" TERM=dumb \
  $( [ -z "${NO_CFG:-}" ] && echo "CLAUDE_CONFIG_DIR=$CFG" ) ANTHROPIC_API_KEY=sk-ant-dummy-capture \
  ANTHROPIC_BASE_URL="http://127.0.0.1:$PORT" \
  CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 DISABLE_AUTOUPDATER=1 \
  ${EXTRA_ENV:-} \
  perl -e "alarm 90; exec @ARGV" /Users/sungjin/.local/bin/claude -p "say hi" --debug-file "$EXP/debug.log" "$@" \
  > "$EXP/stdout.txt" 2> "$EXP/stderr.txt"
echo "exit=$?" > "$EXP/exit.txt"
kill $SRV 2>/dev/null
echo "$EXP"
