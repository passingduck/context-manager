#!/usr/bin/env bash
# Model-checks the write pipeline. The skill .claude/skills/write-pipeline/SKILL.md says
# what the result means.
#
#   formal/write-pipeline/run.sh              the model, every broken variant, every finding
#   formal/write-pipeline/run.sh NAME...      only these: `model`, a variant or a finding name
#   TRACE=1 formal/write-pipeline/run.sh ...  also print the trace of each expected failure
#   OUT=dir formal/write-pipeline/run.sh ...  also keep TLC's output of each run in dir
#
# The first run downloads a pinned Java runtime and TLC into .cache/formal, which git
# ignores, and checks each download against the SHA-256 written below. It installs
# nothing anywhere else.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
model="$root/formal/write-pipeline"
cache="$root/.cache/formal"

tla_url="https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar"
tla_sha="936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88"

# Eclipse Temurin 21.0.5+11 JRE. The checksums are the ones the release publishes.
jre_release="https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.5%2B11"
jre_dir="jdk-21.0.5+11-jre"
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)
    jre_file="OpenJDK21U-jre_aarch64_mac_hotspot_21.0.5_11.tar.gz"
    jre_sha="12249a1c5386957c93fc372260c483ae921b1ec6248a5136725eabd0abc07f93"
    java_rel="Contents/Home/bin/java" ;;
  Darwin-x86_64)
    jre_file="OpenJDK21U-jre_x64_mac_hotspot_21.0.5_11.tar.gz"
    jre_sha="0e0dcb571f7bf7786c111fe066932066d9eab080c9f86d8178da3e564324ee81"
    java_rel="Contents/Home/bin/java" ;;
  Linux-x86_64)
    jre_file="OpenJDK21U-jre_x64_linux_hotspot_21.0.5_11.tar.gz"
    jre_sha="553dda64b3b1c3c16f8afe402377ffebe64fb4a1721a46ed426a91fd18185e62"
    java_rel="bin/java" ;;
  Linux-aarch64)
    jre_file="OpenJDK21U-jre_aarch64_linux_hotspot_21.0.5_11.tar.gz"
    jre_sha="e4d02c33aeaf8e1148c1c505e129a709c5bc1889e855d4fb4f001b1780db42b4"
    java_rel="bin/java" ;;
  *)
    echo "run.sh: no pinned Java runtime for $(uname -s)-$(uname -m)" >&2
    exit 2 ;;
esac

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# fetch URL SHA256 DEST: download once, refuse a file whose hash differs.
fetch() {
  local url="$1" want="$2" dest="$3"
  if [ -f "$dest" ] && [ "$(sha256 "$dest")" = "$want" ]; then
    return
  fi
  mkdir -p "$(dirname "$dest")"
  echo "run.sh: downloading $url" >&2
  curl -fsSL --retry 3 -o "$dest.part" "$url"
  local got
  got="$(sha256 "$dest.part")"
  if [ "$got" != "$want" ]; then
    rm -f "$dest.part"
    echo "run.sh: SHA-256 of $url is $got, expected $want" >&2
    exit 2
  fi
  mv "$dest.part" "$dest"
}

jar="$cache/tla2tools-1.7.4.jar"
fetch "$tla_url" "$tla_sha" "$jar"
java="$cache/$jre_dir/$java_rel"
if [ ! -x "$java" ]; then
  fetch "$jre_release/$jre_file" "$jre_sha" "$cache/$jre_file"
  tar -xzf "$cache/$jre_file" -C "$cache"
  rm -f "$cache/$jre_file"
fi

# Each configuration and the result it must have: `pass`, or the invariant it must break.
# A variant mirrors a break of the code and must fail. A finding is a case where the code
# at the sync commit breaks a property; it fails until the code is fixed.
configs=("model:WritePipeline.cfg:pass")
for kind in variant finding; do
  for cfg in "$model/${kind}s"/*.cfg; do
    [ -e "$cfg" ] || continue
    expect="$(sed -n 's/^\\\* expect: *//p' "$cfg" | head -n 1)"
    if [ -z "$expect" ]; then
      echo "run.sh: $cfg has no '\\* expect: <Invariant>' line" >&2
      exit 2
    fi
    configs+=("$(basename "$cfg" .cfg):${kind}s/$(basename "$cfg"):$expect")
  done
done

wanted=("$@")
selected() {
  [ "${#wanted[@]}" -eq 0 ] && return 0
  local w
  for w in "${wanted[@]}"; do [ "$w" = "$1" ] && return 0; done
  return 1
}

# Every TLC run works in its own directory under one temp directory: a copy of the spec,
# its configuration, TLC's state files and its output. The directory goes when the script
# exits, also on failure or interrupt. A run that passes the disk or time limit is killed.
limit_kb=$(( ${LIMIT_MB:-400} * 1024 ))
limit_s=${LIMIT_S:-300}
tmp="$(mktemp -d "${TMPDIR:-/tmp}/write-pipeline.XXXXXX")"
tlc_pid=""
cleanup() {
  [ -n "$tlc_pid" ] && kill "$tlc_pid" 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# run_tlc NAME CFG: runs TLC on CFG in $tmp/NAME. Sets `peak_kb` and `killed`.
run_tlc() {
  local work="$tmp/$1"
  mkdir -p "$work"
  cp "$model/WritePipeline.tla" "$work/"
  cp "$model/$2" "$work/run.cfg"
  (cd "$work" && exec "$java" -XX:+UseParallelGC -cp "$jar" tlc2.TLC -workers 1 \
      -checkpoint 0 -config run.cfg -metadir "$work/states" WritePipeline.tla) \
      >"$work/tlc.out" 2>&1 &
  tlc_pid=$!
  local begin used
  begin=$(date +%s)
  peak_kb=0
  killed=""
  while kill -0 "$tlc_pid" 2>/dev/null; do
    used=$(du -sk "$work" | cut -f1)
    [ "$used" -gt "$peak_kb" ] && peak_kb=$used
    if [ "$used" -gt "$limit_kb" ]; then
      killed="disk use passed ${LIMIT_MB:-400} MB"
    elif [ $(( $(date +%s) - begin )) -gt "$limit_s" ]; then
      killed="run time passed ${limit_s}s"
    fi
    if [ -n "$killed" ]; then
      kill "$tlc_pid" 2>/dev/null || true
      break
    fi
    sleep 1
  done
  wait "$tlc_pid" 2>/dev/null || true
  tlc_pid=""
}

errors=0
total_start=$(date +%s)
total_peak_kb=0
for entry in "${configs[@]}"; do
  IFS=: read -r name cfg expect <<<"$entry"
  selected "$name" || continue
  start=$(date +%s)
  run_tlc "$name" "$cfg"
  out="$tmp/$name/tlc.out"
  secs=$(( $(date +%s) - start ))
  [ "$peak_kb" -gt "$total_peak_kb" ] && total_peak_kb=$peak_kb
  states="$(sed -n 's/.* states generated, \([0-9]*\) distinct states found.*/\1/p' "$out" | tail -n 1)"
  if [ -n "$killed" ]; then
    got="error"
  elif grep -q "Model checking completed. No error has been found." "$out"; then
    got="pass"
  elif broke="$(sed -n 's/^Error: Invariant \([A-Za-z]*\) is violated.*/\1/p' "$out" | head -n 1)" \
       && [ -n "$broke" ]; then
    got="$broke"
  else
    got="error"
  fi
  label="$name"
  [ "$cfg" = "WritePipeline.cfg" ] || label="${cfg%%s/*} $name"
  summary="$(printf '%-30s %-32s %9s states %4ss %5s MB' "$label" \
    "$([ "$got" = pass ] && echo passes || echo "breaks $got")" "${states:-?}" "$secs" \
    "$(( peak_kb / 1024 ))")"
  if [ "$got" = "$expect" ]; then
    echo "ok    $summary"
    if [ "$got" != pass ] && [ -n "${TRACE:-}" ]; then
      node "$model/trace.mjs" "$model/WritePipeline.tla" "$out"
    fi
  else
    errors=$((errors + 1))
    echo "FAIL  $summary   (expected: $expect)"
    if [ -n "$killed" ]; then
      echo "      killed: $killed. Make the bounds in $cfg smaller."
    elif [ "$got" = error ]; then
      grep -v '^\s*$' "$out" | tail -n 30
    elif [ "$got" != pass ]; then
      node "$model/trace.mjs" "$model/WritePipeline.tla" "$out"
    fi
  fi
  if [ -n "${OUT:-}" ]; then
    mkdir -p "$OUT"
    cp "$out" "$OUT/$name.out"
  fi
  rm -rf "${tmp:?}/$name"
done

echo "run.sh: $(( $(date +%s) - total_start ))s in total, peak disk use of a run $(( total_peak_kb / 1024 )) MB"
if [ "$errors" -gt 0 ]; then
  echo "run.sh: $errors configuration(s) did not give the expected result" >&2
  exit 1
fi
echo "run.sh: ok"
