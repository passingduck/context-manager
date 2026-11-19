"""Probe Codex CLI context discovery with `codex debug prompt-input`.

Each scenario builds a throwaway CODEX_HOME + HOME + project tree under the
scratchpad, runs the real installed binary, and asserts on the model-visible
prompt it renders. Nothing under the real ~/.codex is read or written.
Run: python3 -I probe_codex.py
"""

import json
import os
import shutil
import subprocess
import sys

BASE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "probe_runs")


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)


def setup(name):
    root = os.path.join(BASE, name)
    shutil.rmtree(root, ignore_errors=True)
    home = os.path.join(root, "home")
    codex_home = os.path.join(home, ".codex")
    proj = os.path.join(root, "proj")
    os.makedirs(codex_home)
    os.makedirs(os.path.join(proj, "sub", "deep"))
    subprocess.run(["git", "init", "-q", proj], check=True)  # real repo: .git/HEAD exists
    return root, home, codex_home, proj


def run(home, codex_home, cwd, extra_args=()):
    env = dict(os.environ, HOME=home, CODEX_HOME=codex_home)
    p = subprocess.run(
        ["codex", "debug", "prompt-input", *extra_args],
        cwd=cwd, env=env, capture_output=True, text=True, timeout=120,
    )
    if p.returncode != 0:
        return None, p.stderr.strip()[-800:]
    return json.loads(p.stdout), p.stderr


def texts(items):
    out = []
    for it in items or []:
        for c in it.get("content", []) or []:
            if isinstance(c, dict) and "text" in c:
                out.append((it.get("role"), c["text"]))
    return out


def agents_block(items):
    for role, t in texts(items):
        if t.startswith("# AGENTS.md instructions"):
            return t
    return ""


def all_text(items):
    return "\n".join(t for _, t in texts(items))


RESULTS = []


def check(name, cond, detail=""):
    RESULTS.append((name, bool(cond), detail))


def trust(codex_home, proj, level):
    write(os.path.join(codex_home, "config.toml"),
          f'[projects."{os.path.realpath(proj)}"]\ntrust_level = "{level}"\n')


# A: baseline walk, below-cwd excluded, global trimmed, separator
r, h, ch, p = setup("A")
write(os.path.join(ch, "AGENTS.md"), "\n  GLOBAL\n\n")
write(os.path.join(p, "AGENTS.md"), "ROOT\n")
write(os.path.join(p, "sub", "AGENTS.md"), "SUB\n")
write(os.path.join(p, "sub", "deep", "AGENTS.md"), "DEEP\n")
items, err = run(h, ch, os.path.join(p, "sub"))
b = agents_block(items)
check("A root->cwd order, below-cwd skipped", b.find("ROOT") < b.find("SUB") and "DEEP" not in b and "GLOBAL" in b, b)
check("A separator between global and project", "GLOBAL\n\n--- project-doc ---\n\nROOT" in b, repr(b[-120:]))
check("A untrusted-unknown project still loads AGENTS.md", "ROOT" in b)

# B: empty AGENTS.override.md shadows AGENTS.md in the same directory
r, h, ch, p = setup("B")
write(os.path.join(p, "AGENTS.md"), "ROOT\n")
write(os.path.join(p, "sub", "AGENTS.md"), "SUB\n")
write(os.path.join(p, "sub", "AGENTS.override.md"), "   \n")
items, err = run(h, ch, os.path.join(p, "sub"))
b = agents_block(items)
check("B empty project override shadows AGENTS.md", "ROOT" in b and "SUB" not in b, b)

# C: empty global override falls through to global AGENTS.md
r, h, ch, p = setup("C")
write(os.path.join(ch, "AGENTS.override.md"), "\n")
write(os.path.join(ch, "AGENTS.md"), "GLOBAL\n")
items, err = run(h, ch, p)
b = agents_block(items)
check("C empty global override falls back to AGENTS.md", "GLOBAL" in b, b or err)
write(os.path.join(ch, "AGENTS.override.md"), "GOVERRIDE\n")
items, err = run(h, ch, p)
b = agents_block(items)
check("C non-empty global override wins", "GOVERRIDE" in b and "GLOBAL\n" not in b.replace("GOVERRIDE", ""), b)

# D: explicitly untrusted project -> no project docs, global kept
r, h, ch, p = setup("D")
write(os.path.join(ch, "AGENTS.md"), "GLOBAL\n")
write(os.path.join(p, "AGENTS.md"), "ROOT\n")
trust(ch, p, "untrusted")
items, err = run(h, ch, p)
b = agents_block(items)
check("D untrusted project skips project AGENTS.md", "GLOBAL" in b and "ROOT" not in b, b or err)

# E: project_doc_max_bytes truncates mid-file, then stops; global not counted
r, h, ch, p = setup("E")
write(os.path.join(ch, "AGENTS.md"), "G" * 40 + "\n")
write(os.path.join(p, "AGENTS.md"), "0123456789ABCDEF\n")
write(os.path.join(p, "sub", "AGENTS.md"), "SUBFILE\n")
write(os.path.join(ch, "config.toml"), "project_doc_max_bytes = 10\n")
items, err = run(h, ch, os.path.join(p, "sub"))
b = agents_block(items)
check("E truncates first file to budget", "0123456789\n" in b and "ABCDEF" not in b, repr(b))
check("E later files dropped once budget spent", "SUBFILE" not in b, repr(b))
check("E global not counted against budget", "G" * 40 in b, repr(b))

# F: fallback filename used only when no AGENTS.md / override in that dir
r, h, ch, p = setup("F")
write(os.path.join(ch, "config.toml"), 'project_doc_fallback_filenames = ["TEAM.md"]\n')
write(os.path.join(p, "TEAM.md"), "TEAMROOT\n")
write(os.path.join(p, "sub", "TEAM.md"), "TEAMSUB\n")
write(os.path.join(p, "sub", "AGENTS.md"), "SUB\n")
write(os.path.join(ch, "TEAM.md"), "TEAMGLOBAL\n")
items, err = run(h, ch, os.path.join(p, "sub"))
b = agents_block(items)
check("F fallback used where AGENTS.md absent", "TEAMROOT" in b, b or err)
check("F AGENTS.md beats fallback in same dir", "SUB" in b and "TEAMSUB" not in b, b)
check("F fallback not applied to global", "TEAMGLOBAL" not in b, b)

# G: no marker -> only cwd
r, h, ch, p = setup("G")
shutil.rmtree(os.path.join(p, ".git"))
write(os.path.join(p, "AGENTS.md"), "ROOT\n")
write(os.path.join(p, "sub", "AGENTS.md"), "SUB\n")
items, err = run(h, ch, os.path.join(p, "sub"))
b = agents_block(items)
check("G no root marker -> cwd only", "SUB" in b and "ROOT" not in b, b or err)

# H: project_root_markers in a trusted project .codex/config.toml is ignored for
# AGENTS.md root detection; the same key in the user layer is honored.
r, h, ch, p = setup("H")
write(os.path.join(p, "sub", ".hg"), "")
write(os.path.join(p, "AGENTS.md"), "ROOT\n")
write(os.path.join(p, "sub", "AGENTS.md"), "SUB\n")
write(os.path.join(p, ".codex", "config.toml"), 'project_root_markers = [".hg"]\ndeveloper_instructions = "H-LAYER-LOADED"\n')
trust(ch, p, "trusted")
items, err = run(h, ch, os.path.join(p, "sub", "deep"))
b = agents_block(items)
check("H control: trusted project layer is loaded", "H-LAYER-LOADED" in all_text(items), err)
check("H project-layer project_root_markers ignored", "ROOT" in b and "SUB" in b, b or err)
write(os.path.join(ch, "config.toml"),
      f'project_root_markers = [".hg"]\n[projects."{os.path.realpath(p)}"]\ntrust_level = "trusted"\n')
items, err = run(h, ch, os.path.join(p, "sub", "deep"))
b = agents_block(items)
check("H user-layer marker (a plain file) honored", "ROOT" not in b and "SUB" in b, b or err)

# Q: a .git directory without HEAD counts as an AGENTS.md root marker but not
# as a config-layer root marker.
r, h, ch, p = setup("Q")
shutil.rmtree(os.path.join(p, ".git"))
os.makedirs(os.path.join(p, ".git"))
write(os.path.join(p, "AGENTS.md"), "ROOT\n")
write(os.path.join(p, ".codex", "config.toml"), 'developer_instructions = "Q-LAYER-LOADED"\n')
trust(ch, p, "trusted")
items, err = run(h, ch, os.path.join(p, "sub"))
check("Q fake .git dir: AGENTS.md walk reaches it", "ROOT" in agents_block(items), err)
check("Q fake .git dir: root .codex layer not loaded from sub", "Q-LAYER-LOADED" not in all_text(items), err)
items, err = run(h, ch, p)
check("Q control: root .codex layer loads when cwd is the root", "Q-LAYER-LOADED" in all_text(items), err)

# I: project .codex/config.toml developer_instructions gated by trust
r, h, ch, p = setup("I")
write(os.path.join(p, ".codex", "config.toml"), 'developer_instructions = "PROJ-DEV-INSTR"\n')
items, err = run(h, ch, p)
check("I unknown-trust project config ignored", "PROJ-DEV-INSTR" not in all_text(items), err)
trust(ch, p, "trusted")
items, err = run(h, ch, p)
dev = [t for role, t in texts(items) if "PROJ-DEV-INSTR" in t]
check("I trusted project developer_instructions injected", bool(dev), err)
roles = [role for role, t in texts(items) if "PROJ-DEV-INSTR" in t]
check("I developer_instructions role is developer", roles and all(x == "developer" for x in roles), str(roles))

# J: skills roots
r, h, ch, p = setup("J")
skill = "---\nname: {n}\ndescription: {n} description\n---\nbody\n"
write(os.path.join(p, ".agents", "skills", "rootskill", "SKILL.md"), skill.format(n="rootskill"))
write(os.path.join(p, "sub", ".agents", "skills", "subskill", "SKILL.md"), skill.format(n="subskill"))
write(os.path.join(p, "sub", "deep", ".agents", "skills", "deepskill", "SKILL.md"), skill.format(n="deepskill"))
write(os.path.join(p, ".codex", "skills", "dotcodexskill", "SKILL.md"), skill.format(n="dotcodexskill"))
write(os.path.join(ch, "skills", "codexhomeskill", "SKILL.md"), skill.format(n="codexhomeskill"))
write(os.path.join(h, ".agents", "skills", "homeagentsskill", "SKILL.md"), skill.format(n="homeagentsskill"))
write(os.path.join(ch, "skills", "nodesc", "SKILL.md"), "---\nname: nodesc\n---\nbody\n")
write(os.path.join(ch, "skills", "a", "b", "c", "nested", "SKILL.md"), skill.format(n="nested"))
items, err = run(h, ch, os.path.join(p, "sub"))
t = all_text(items)
for n in ["rootskill", "subskill", "codexhomeskill", "homeagentsskill", "nested"]:
    check(f"J skill {n} listed", f"- {n}:" in t, err)
check("J skill below cwd not listed", "- deepskill:" not in t)
check("J unknown-trust project .codex/skills still listed (disabled layer)", "- dotcodexskill:" in t)
trust(ch, p, "untrusted")
items, err = run(h, ch, os.path.join(p, "sub"))
t2 = all_text(items)
check("J explicitly untrusted: .codex/skills still listed", "- dotcodexskill:" in t2, err)
check("J explicitly untrusted: .agents/skills still listed", "- rootskill:" in t2, err)
trust(ch, p, "trusted")
items, err = run(h, ch, os.path.join(p, "sub"))
check("J trusted project .codex/skills listed", "- dotcodexskill:" in all_text(items), err)
check("J skill without description rejected", "- nodesc:" not in t)
check("J SKILL.md body not in context", "body" not in t.split("<skills_instructions>")[-1].split("</skills_instructions>")[0])

# K: memories. `codex debug prompt-input` does not install the memories
# extension (cli/src/main.rs builds its own registry; only app-server's
# extensions.rs installs codex_memories_extension), so injection cannot be
# observed here. We only check that memory files never leak in this path.
r, h, ch, p = setup("K")
write(os.path.join(ch, "memories", "memory_summary.md"), "MEMSUMMARY-MARK\n")
write(os.path.join(ch, "memories", "MEMORY.md"), "MEMORY-REGISTRY-MARK\n")
items, err = run(h, ch, p, ["--enable", "memories"])
t = all_text(items)
check("K MEMORY.md never injected", "MEMORY-REGISTRY-MARK" not in t, err)

# L: model_instructions_file replaces base instructions
r, h, ch, p = setup("L")
write(os.path.join(ch, "base.md"), "CUSTOM-BASE-MARK\n")
write(os.path.join(ch, "config.toml"), f'model_instructions_file = "{os.path.join(ch, "base.md")}"\n')
items, err = run(h, ch, p)
t = all_text(items)
check("L model_instructions_file replaces built-in base", "CUSTOM-BASE-MARK" in t and "You are Codex" not in t, err)

# M: custom agent role description visible to the main model?
r, h, ch, p = setup("M")
write(os.path.join(ch, "agents", "probe.toml"),
      'name = "probe_role"\ndescription = "AGENT-DESC-MARK"\ndeveloper_instructions = "AGENT-DEV-MARK"\n')
items, err = run(h, ch, p)
t = all_text(items)
check("M agent developer_instructions not in main prompt", "AGENT-DEV-MARK" not in t, err)
check("M agent description not in prompt messages (tool spec only)", "AGENT-DESC-MARK" not in t)

# O: symlinked AGENTS.md is followed; a directory named AGENTS.md is skipped
r, h, ch, p = setup("O")
write(os.path.join(r, "elsewhere", "real.md"), "SYMLINKED\n")
os.symlink(os.path.join(r, "elsewhere", "real.md"), os.path.join(p, "AGENTS.md"))
os.makedirs(os.path.join(p, "sub", "AGENTS.override.md"))
write(os.path.join(p, "sub", "AGENTS.md"), "SUB\n")
items, err = run(h, ch, os.path.join(p, "sub"))
b = agents_block(items)
check("O symlinked AGENTS.md followed", "SYMLINKED" in b, b or err)
check("O directory named AGENTS.override.md skipped", "SUB" in b, b)

# N: global AGENTS.md larger than 32 KiB is not truncated
r, h, ch, p = setup("N")
write(os.path.join(ch, "AGENTS.md"), "x" * 40000 + "END-GLOBAL\n")
items, err = run(h, ch, p)
check("N global AGENTS.md > 32KiB not truncated", "END-GLOBAL" in agents_block(items), err)

# R: no include syntax; CLAUDE.md is not a default name
r, h, ch, p = setup("R")
write(os.path.join(p, "AGENTS.md"), "@inc.md\nSee @./inc.md\n")
write(os.path.join(p, "inc.md"), "INCLUDED-MARK\n")
write(os.path.join(p, "CLAUDE.md"), "CLAUDE-MARK\n")
items, err = run(h, ch, p)
b = agents_block(items)
check("R @-references passed verbatim, not inlined", "@inc.md" in b and "INCLUDED-MARK" not in b, b or err)
check("R CLAUDE.md not loaded by default", "CLAUDE-MARK" not in all_text(items), "")

for name, ok, detail in RESULTS:
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else "  :: " + detail[:400].replace("\n", "\\n")))
