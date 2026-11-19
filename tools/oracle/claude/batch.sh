#!/bin/bash
# Runs the discovery scenarios in parallel. Each scenario gets its own fixture dir and port.
T=/private/tmp/claude-501/-Users-sungjin-context-manger/dddccb01-94a9-4428-bd2b-2f11d7667ddd/scratchpad/research/tools
cd "$T" || exit 1

./exp.sh srcuser --setting-sources user >/dev/null &
EXTRA_ENV="CLAUDE_CODE_DISABLE_CLAUDE_MDS=1" ./exp.sh disable >/dev/null &
EXTRA_ENV="CLAUDE_CODE_DISABLE_AUTO_MEMORY=1" ./exp.sh nomem >/dev/null &

ADD='mkdir -p "$EXP/extra/.claude/rules"; printf "MARK-ADD-CLAUDE\n" > "$EXP/extra/CLAUDE.md"; printf "MARK-ADD-DOTCLAUDE\n" > "$EXP/extra/.claude/CLAUDE.md"; printf "MARK-ADD-LOCAL\n" > "$EXP/extra/CLAUDE.local.md"; printf "MARK-ADD-RULE\n" > "$EXP/extra/.claude/rules/x.md"; printf -- "---\npaths: \"**/*.go\"\n---\nMARK-ADD-RULE-GO\n" > "$EXP/extra/.claude/rules/y.md"; printf "MARK-ADD-AGENTS\n" > "$EXP/extra/AGENTS.md"'
PRE="$ADD" EXTRA_ENV="CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD=1" ./exp.sh adddir_on --add-dir ../../../extra >/dev/null &
PRE="$ADD" ./exp.sh adddir_off --add-dir ../../../extra >/dev/null &

PRE='python3 -I -c "import json,sys; p=sys.argv[1]; d=json.load(open(p)); d[\"claudeMdExcludes\"]=[\"**/top/CLAUDE.md\",\"**/.claude/rules/sub/**\"]; json.dump(d,open(p,\"w\"))" "$CFG/settings.json"' ./exp.sh excludes >/dev/null &

PRE='rm -f "$TOP/CLAUDE.md" "$REPO/CLAUDE.md" "$REPO/.claude/CLAUDE.md" "$REPO/CLAUDE.local.md" "$APP/CLAUDE.md"; printf "MARK-REPO-DOTCLAUDE-AGENTS\n" > "$REPO/.claude/AGENTS.md"; printf "MARK-APP-AGENTS\n" > "$APP/AGENTS.md"; printf "MARK-APP-AGENTS-LOCAL\n" > "$APP/AGENTS.local.md"; printf "MARK-APP-AGENTS-OVERRIDE\n" > "$APP/AGENTS.override.md"; mkdir -p "$APP/.agents"; printf "MARK-DOT-AGENTS\n" > "$APP/.agents/AGENTS.md"' ./exp.sh agents >/dev/null &

PRE='python3 -I -c "import json,sys; p=sys.argv[1]; d=json.load(open(p)); d[\"autoMemoryDirectory\"]=sys.argv[2]; json.dump(d,open(p,\"w\"))" "$CFG/settings.json" "$EXP/custommem"; mkdir -p "$EXP/custommem"; printf "MARK-CUSTOM-MEMDIR\n" > "$EXP/custommem/MEMORY.md"' ./exp.sh memdir >/dev/null &

PRE='python3 -I -c "import sys; open(sys.argv[1],\"w\").write(\"\".join(\"- MARK-BYTES-%03d \" % i + \"x\"*280 + \"\n\" for i in range(1,151)))" "$MEM/MEMORY.md"; python3 -I -c "import sys; open(sys.argv[1],\"w\").write(\"MARK-BIG-HEAD\n\" + \"y\"*(4*1024*1024) + \"\nMARK-BIG-TAIL\n\")" "$APP/CLAUDE.md"' ./exp.sh bytes >/dev/null &

PRE='mkdir -p "$EXP/shared/rules"; printf "MARK-SHARED-EXT-RULE\n" > "$EXP/shared/rules/s1.md"; ln -s "$EXP/shared/rules" "$REPO/.claude/rules/shared"; mkdir -p "$REPO/inrepo"; printf "MARK-INREPO-LINKED-RULE\n" > "$REPO/inrepo/r.md"; ln -s "$REPO/inrepo/r.md" "$REPO/.claude/rules/linked.md"' CWD_REL=top/repo ./exp.sh symlinks >/dev/null &

PRE='( cd "$REPO" && git -c user.email=x@y -c user.name=x add -A >/dev/null && git -c user.email=x@y -c user.name=x commit -qm init && git worktree add -q "$EXP/wt" -b wt && mkdir -p .claude/worktrees && git worktree add -q "$REPO/.claude/worktrees/nested" -b nested ); printf "MARK-WT-LOCAL-UNTRACKED\n" > "$EXP/wt/CLAUDE.local.md"' CWD_REL=wt ./exp.sh worktree >/dev/null &
PRE='( cd "$REPO" && git -c user.email=x@y -c user.name=x add -A >/dev/null && git -c user.email=x@y -c user.name=x commit -qm init && git worktree add -q "$EXP/wt" -b wt && mkdir -p .claude/worktrees && git worktree add -q "$REPO/.claude/worktrees/nested" -b nested ); printf "MARK-WT-LOCAL-UNTRACKED\n" > "$EXP/wt/CLAUDE.local.md"' CWD_REL=top/repo/.claude/worktrees/nested ./exp.sh wtnested >/dev/null &

wait
echo done
