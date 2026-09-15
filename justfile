# sccache (optional): when installed, cargo compiles through it so dependency
# crates are served from its cache instead of being recompiled. Builds fall
# back to plain rustc when sccache is missing, so it is never required. A
# caller-provided RUSTC_WRAPPER (set, or explicitly empty to disable) wins.
export RUSTC_WRAPPER := env_var_or_default('RUSTC_WRAPPER', `command -v sccache 2>/dev/null || true`)

COPYRIGHT_NAME := "Jayson Lennon"
COPYRIGHT_YEAR := "2026"

fossil-branch NAME:
    fossil commit -m "Open {{NAME}}" --branch {{NAME}} --allow-empty

# Commit changes (ONE LINE ONLY): stage all adds/removes (with `--dotfiles`) and commit.
commit MSG:
    fossil addremove --dotfiles && fossil commit -m "{{MSG}}"

test:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p target
    # One suite run; full output captured so failure details never require a
    # re-run. tee must not mask cargo's exit code (pipefail handles it).
    set -o pipefail
    if ! cargo test --workspace --no-fail-fast 2>&1 | tee target/test-output.log; then
        TEST_STATUS="failed"
    else
        TEST_STATUS="passed"
    fi
    echo ""
    echo "==> Test summary ($(date -u +%Y-%m-%dT%H:%M:%SZ)) — full log: target/test-output.log"
    awk '/^test result:/ {ok+=$4; fail+=$6} END {printf "    passed: %d  failed: %d\n", ok, fail}' target/test-output.log
    if [ "$TEST_STATUS" = "failed" ]; then
        echo "    Failing tests (details: just test-failures):"
        awk '/^failures:$/{f=1; next} f && /^    /{gsub(/^    /,""); print "      - " $0; found=1; next} f && NF && !/^    /{f=0} END{if(!found) print "      (no named failures in log; check test-output.log)"}' target/test-output.log | sort -u
        exit 1
    fi

# Show failing tests from the last `just test` run WITHOUT re-running anything.
test-failures:
    #!/usr/bin/env bash
    set -euo pipefail
    LOG=target/test-output.log
    if [ ! -f "$LOG" ]; then
        echo 'No test log found at target/test-output.log — run `just test` first.' >&2
        exit 1
    fi
    if grep -qE '^test result: FAILED|^error: could not compile' "$LOG"; then
        grep -E '^failures:$' -A 100 "$LOG" | grep -E '^    [a-z_]+' | sed 's/^    //' | sort -u
    else
        echo "Last run passed — no failures in $LOG."
    fi

# Run only tests whose name matches FILTER, across the whole workspace.
# Sanctioned iterate-on-failure loop: fix code, re-run `just test-one <name>`,
# then confirm with a final `just test` before committing.
test-one FILTER:
    cargo test --workspace --no-fail-fast {{FILTER}}

check:
    cargo check --workspace

# Report release-binary size: bytes, section table, per-crate .text (cargo-bloat), upx size
size-report:
    #!/usr/bin/env bash
    set -euo pipefail

    BIN=target/release/jinn
    cargo build --release

    echo "==> Binary: $BIN"
    ls -la "$BIN" | awk '{printf "    %s bytes\n", $5}'
    du -h "$BIN" | awk '{print "    " $1 " on disk"}'

    echo '==> Section table (largest first):'
    size -A "$BIN" | tail -n +3 | sort -k2 -n -r | head -12 | awk '{printf "    %-22s %10d bytes\n", $2, $3}' || true

    if command -v cargo-bloat >/dev/null 2>&1; then
        echo '==> Top crates by .text (cargo-bloat):'
        cargo bloat --release -n 15 --crates 2>/dev/null | tail -n +3 || true
    else
        echo '    (cargo-bloat not installed; skipping per-crate breakdown)'
    fi

    if command -v upx >/dev/null 2>&1; then
        TMP="$(mktemp)"
        cp "$BIN" "$TMP"
        if upx -q "$TMP" >/dev/null 2>&1; then
            UPX_SIZE=$(stat -c %s "$TMP")
            RAW_SIZE=$(stat -c %s "$BIN")
            echo '==> UPX-packed estimate:'
            awk -v r="$RAW_SIZE" -v p="$UPX_SIZE" 'BEGIN{printf "    %d bytes (%.1f%% of raw)\n", p, p*100.0/r}'
        fi
        rm -f "$TMP"
    else
        echo '    (upx not installed; skipping packed-size estimate)'
    fi

# Build + install every in-tree plugin via `jinn plugin add` (interleaved per plugin; aborts at first failure)
install-plugins:
    #!/usr/bin/env bash
    set -euo pipefail

    if ! command -v cargo >/dev/null 2>&1; then
        echo "Error: cargo is not installed." >&2; exit 1
    fi
    if ! rustup target list --installed 2>/dev/null | grep -q 'wasm32-wasip2'; then
        echo "Error: wasm32-wasip2 target not installed." >&2
        echo "  Run: rustup target add wasm32-wasip2" >&2
        exit 1
    fi

    echo '==> Ensuring jinn binary'
    [ -x target/release/jinn ] || cargo build --release -p jinn

    shopt -s nullglob
    manifests=(plugins/*/Cargo.toml)
    if [ ${#manifests[@]} -eq 0 ]; then
        echo "No plugins found under plugins/" >&2; exit 1
    fi
    for manifest in "${manifests[@]}"; do
        dir="$(dirname "$manifest")"
        echo "==> Installing $dir"
        target/release/jinn plugin add "$dir"
    done

# Build every in-tree wasm plugin (needs wasm32-wasip2 target + jinn binary); see plugins/*/Cargo.toml
build-plugins:
    #!/usr/bin/env bash
    set -euo pipefail

    if ! command -v cargo >/dev/null 2>&1; then
        echo "Error: cargo is not installed." >&2; exit 1
    fi
    if ! rustup target list --installed 2>/dev/null | grep -q 'wasm32-wasip2'; then
        echo "Error: wasm32-wasip2 target not installed." >&2
        echo "  Run: rustup target add wasm32-wasip2" >&2
        exit 1
    fi

    echo '==> Ensuring jinn binary'
    [ -x target/release/jinn ] || cargo build --release -p jinn

    shopt -s nullglob
    manifests=(plugins/*/Cargo.toml)
    if [ ${#manifests[@]} -eq 0 ]; then
        echo "No plugins found under plugins/" >&2; exit 1
    fi
    for manifest in "${manifests[@]}"; do
        dir="$(dirname "$manifest")"
        echo "==> Building $dir"
        target/release/jinn plugin build "$dir"
    done

# Rebuild plugins and copy artifacts into res/plugins/ (embedded payloads; run before `just release`)
refresh-plugins: build-plugins
    #!/usr/bin/env bash
    set -euo pipefail

    mkdir -p res/plugins
    shopt -s nullglob
    manifests=(plugins/*/Cargo.toml)
    for manifest in "${manifests[@]}"; do
        name="$(basename "$(dirname "$manifest")")"
        artifact="target/wasm32-wasip2/release/${name}.wasm"
        echo "==> Refreshing res/plugins/${name}.wasm"
        cp "$artifact" "res/plugins/${name}.wasm"
    done


clippy:
    cargo clippy --workspace --all-targets

fmt:
    cargo fmt -- --check

fmt-fix:
    cargo fmt

# Run all linters (check + clippy + fmt check + test-attr guard)
lint:
    cargo check --workspace
    just clippy
    cargo fmt -- --check
    just lint-testattr

# Fail on bare #[test]/#[tokio::test] lacking an rstest attr (escapes the rstest timeout)
lint-testattr:
   #!/usr/bin/env python3
   import os
   import re
   import sys

   ALLOWLIST = {
       os.path.normpath("crates/jinn-domain/tests/tcaps_compile_fail.rs"),
   }
   SKIP_DIRS = {"target", "vendor"}

   def is_test_attr(s):
       if re.fullmatch(r"#\[test\]", s):
           return True
       return bool(re.fullmatch(r"#\[tokio::test(?:\([^()]*(?:\([^()]*\)[^()]*)?\))?\]", s))

   offenders = []
   for dirpath, dirnames, filenames in os.walk(os.getcwd()):
       dirnames[:] = [d for d in dirnames if d not in SKIP_DIRS]
       for fn in filenames:
           if not fn.endswith(".rs"):
               continue
           fpath = os.path.join(dirpath, fn)
           rel = os.path.normpath(os.path.relpath(fpath, os.getcwd()))
           if rel in ALLOWLIST:
               continue
           with open(fpath, encoding="utf-8") as f:
               lines = f.readlines()
           i, n = 0, len(lines)
           while i < n:
               s = lines[i].strip()
               if s.startswith("#["):
                   # Walk the contiguous attribute region (multi-line ok).
                   depth = 0
                   has_rstest = False
                   has_bare = False
                   bare_line = None
                   j = i
                   while j < n:
                       sj = lines[j].strip()
                       depth += sj.count("[") + sj.count("(")
                       depth -= sj.count("]") + sj.count(")")
                       if "rstest" in sj:
                           has_rstest = True
                       if depth == 0 and is_test_attr(sj):
                           has_bare = True
                           bare_line = j + 1
                       if depth <= 0:
                           nxt = j + 1
                           if nxt < n and lines[nxt].strip().startswith("#["):
                               j = nxt
                               continue
                           break
                       j += 1
                   else:
                       j = n - 1
                   if has_bare and not has_rstest:
                       offenders.append((rel, bare_line))
                   i = j + 1
               else:
                   i += 1

   for rel, ln in offenders:
       print(f"ERROR: {rel}:{ln}: bare test attribute lacks an #[rstest::rstest] companion", file=sys.stderr)
   if offenders:
       print(
           f"\n{len(offenders)} test(s) would run WITHOUT the rstest timeout.\n"
           "Stack #[rstest::rstest] above the attribute (or extend the allowlist\n"
           "in the lint-testattr recipe with a stated reason).",
           file=sys.stderr,
       )
       sys.exit(1)

# Full CI pipeline (lint + test + docs)
ci: lint test
    cargo test --workspace --doc --exclude llm
    cargo doc --workspace --no-deps

# Run all cucumber tests
cucumber:
    cargo test --test e2e -p jinn-e2e

# Rebuild the dao compile-time validation DB (forces jinn-domain build.rs on next check)
dao-db-rebuild:
    cargo clean -p jinn-domain


# Build and open documentation
docs:
    cargo doc --workspace --no-deps --open

coverage:
    cargo llvm-cov --lcov --output-path coverage.lcov

coverage-report:
    cargo llvm-cov report --html

debt: coverage
    debtmap analyze . --lcov coverage.lcov

apply-license:
   #!/bin/bash

   # --- CONFIGURATION ---
   NAME="{{COPYRIGHT_NAME}}"
   YEAR="{{COPYRIGHT_YEAR}}"
   # Add the extensions you want to target (space separated)
   EXTENSIONS=("rs")

   # The Header Template
   HEADER="Copyright (C) $YEAR $NAME

   This program is free software: you can redistribute it and/or modify
   it under the terms of the GNU Affero General Public License as
   published by the Free Software Foundation, either version 3 of the
   License, or (at your option) any later version.

   This program is distributed in the hope that it will be useful,
   but WITHOUT ANY WARRANTY; without even the implied warranty of
   MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
   GNU Affero General Public License for more details.

   You should have received a copy of the GNU Affero General Public License
   along with this program.  If not, see <https://www.gnu.org/licenses/>."

   # Convert header to a commented block (using // for JS/CPP style)
   # If you use Python only, change '// ' to '# ' below.
   COMMENTED_HEADER=$(echo "$HEADER" | sed 's/^/\/\/ /')

   # --- EXECUTION ---
   for ext in "${EXTENSIONS[@]}"; do
       echo "Processing .$ext files..."

       # Find files with the extension, excluding node_modules or hidden git folders
       find . -type f -name "*.$ext" -not -path "*/.*" -not -path "*node_modules*" | while read -r file; do

           # Check if "Copyright" already exists in the first 5 lines
           if head -n 5 "$file" | grep -iq "Copyright"; then
               echo "  Skipping $file (Header already exists)"
           else
               echo "  Adding header to $file"
               # Create a temporary file with header + original content
               { echo "$COMMENTED_HEADER"; echo ""; cat "$file"; } > "$file.tmp" && mv "$file.tmp" "$file"
           fi
       done
   done

   echo "Done!"

# Copy themes, personas, and prompts to user config directory
install-defaults:
    mkdir -p ~/.config/jinn/themes
    cp -r res/themes/*.toml ~/.config/jinn/themes/
    mkdir -p ~/.config/jinn/personas
    cp -r res/personas/*.md ~/.config/jinn/personas/
    mkdir -p ~/.config/jinn/prompts
    cp -r res/prompts/*.md ~/.config/jinn/prompts/
    mkdir -p ~/.agents/skills
    cp -r res/skills/* ~/.agents/skills/
    @echo "Themes installed to ~/.config/jinn/themes/"
    @echo "Personas installed to ~/.config/jinn/personas/"
    @echo "Prompts installed to ~/.config/jinn/prompts/"
    @echo "Skills installed to ~/.agents/skills/"

# Report stale Fossil locks (hung processes + stale journal files)
fossil-unlock:
    #!/usr/bin/env bash
    set -euo pipefail

    REPO="$(fossil status 2>/dev/null | sed -n 's/^repository: *//p')"
    if [ -z "$REPO" ]; then
        echo "Error: not inside a Fossil checkout" >&2; exit 1
    fi

    REPO="$(readlink -f "$REPO")"
    REPO_DIR="$(dirname "$REPO")"
    CHECKOUT_ROOT="$(pwd)"

    # Build the list of DB files worth checking: the repo, the current checkout,
    # and any sibling checkouts that live next to the .fossil file (e.g. ./pins).
    # Locks can live in ANY of these — not just where you're standing.
    TARGETS=("$REPO")
    while IFS= read -r -d '' f; do
        TARGETS+=("$f")
    done < <(find "$CHECKOUT_ROOT" "$REPO_DIR" -maxdepth 2 -name '.fslckout' -print0 2>/dev/null || true)
    # Dedupe (preserve order)
    SEEN=""; FILES=()
    for t in "${TARGETS[@]}"; do
        case "$SEEN" in
            *"|$t|"*) ;;
            *) SEEN="$SEEN|$t|"; FILES+=("$t") ;;
        esac
    done

    FOUND=0

    # 1. Hung Fossil processes holding any target DB
    echo '==> Checking for hung Fossil processes...'
    for db in "${FILES[@]}"; do
        PIDS=$(lsof "$db" 2>/dev/null | awk 'NR>1 && $1=="fossil" {print $2}' | sort -u) || true
        if [ -n "$PIDS" ]; then
            for pid in $PIDS; do
                CMD=$(ps -p "$pid" -o args= 2>/dev/null || echo "<exited>")
                echo "  stale PID $pid on $db: $CMD"
                FOUND=$((FOUND + 1))
            done
        fi
    done
    if [ "$FOUND" -eq 0 ]; then
        echo '  none found'
    fi

    # 2. Stale journal / WAL / SHM files next to each target
    echo '==> Checking for stale journal files...'
    JFOUND=0
    for db in "${FILES[@]}"; do
        d="$(dirname "$db")"
        while IFS= read -r -d '' f; do
            echo "  stale: $f"
            JFOUND=$((JFOUND + 1))
        done < <(find "$d" -maxdepth 1 \
            \( -name '.fslckout-journal' -o -name '.fslckout-wal' -o -name '.fslckout-shm' \
            -o -name '*.fossil-journal' -o -name '*.fossil-wal' -o -name '*.fossil-shm' \) -print0 2>/dev/null || true)
    done
    FOUND=$((FOUND + JFOUND))
    if [ "$JFOUND" -eq 0 ]; then
        echo '  none found'
    fi

    echo ""
    if [ "$FOUND" -gt 0 ]; then
        echo "Found $FOUND issue(s). Run 'just fossil-unlock-fix' to resolve."
    else
        echo "No lock issues found."
    fi

# Fix stale Fossil locks (kill hung processes + remove stale journal files)
fossil-unlock-fix:
    #!/usr/bin/env bash
    set -euo pipefail

    REPO="$(fossil status 2>/dev/null | sed -n 's/^repository: *//p')"
    if [ -z "$REPO" ]; then
        echo "Error: not inside a Fossil checkout" >&2; exit 1
    fi

    REPO="$(readlink -f "$REPO")"
    REPO_DIR="$(dirname "$REPO")"
    CHECKOUT_ROOT="$(pwd)"

    TARGETS=("$REPO")
    while IFS= read -r -d '' f; do
        TARGETS+=("$f")
    done < <(find "$CHECKOUT_ROOT" "$REPO_DIR" -maxdepth 2 -name '.fslckout' -print0 2>/dev/null || true)
    SEEN=""; FILES=()
    for t in "${TARGETS[@]}"; do
        case "$SEEN" in
            *"|$t|"*) ;;
            *) SEEN="$SEEN|$t|"; FILES+=("$t") ;;
        esac
    done

    FIXED=0
    ME="$(whoami)"

    # 1. Kill hung Fossil processes (SIGTERM, escalate to SIGKILL; force stopped procs)
    echo '==> Killing hung Fossil processes...'
    KILLED=0
    for db in "${FILES[@]}"; do
        PIDS=$(lsof "$db" 2>/dev/null | awk 'NR>1 && $1=="fossil" {print $2}' | sort -u) || true
        for pid in $PIDS; do
            OWNER=$(stat -c '%U' "/proc/$pid" 2>/dev/null || echo "")
            if [ "$OWNER" != "$ME" ]; then
                echo "  skipping PID $pid on $db (owned by $OWNER, not $ME)"
                continue
            fi
            if ! kill -0 "$pid" 2>/dev/null; then
                echo "  PID $pid on $db already gone"
                continue
            fi
            CMD=$(ps -p "$pid" -o args= 2>/dev/null || echo "<exited>")
            STATE=$(ps -p "$pid" -o stat= 2>/dev/null | cut -c1 || echo "?")
            if [ "$STATE" = "T" ]; then
                # Stopped/traced: SIGTERM can't be delivered. Force.
                kill -9 "$pid" 2>/dev/null || true
                echo "  force-killed (SIGKILL, was stopped) PID $pid on $db: $CMD"
            else
                kill "$pid" 2>/dev/null || true
                sleep 2
                if kill -0 "$pid" 2>/dev/null; then
                    kill -9 "$pid" 2>/dev/null || true
                    echo "  force-killed (SIGKILL) PID $pid on $db: $CMD"
                else
                    echo "  killed (SIGTERM) PID $pid on $db: $CMD"
                fi
            fi
            KILLED=$((KILLED + 1))
        done
    done
    FIXED=$((FIXED + KILLED))
    if [ "$KILLED" -eq 0 ]; then
        echo '  no hung processes found'
    fi

    # 2. Remove stale journal / WAL / SHM files next to each target
    echo '==> Removing stale journal files...'
    JFIXED=0
    for db in "${FILES[@]}"; do
        d="$(dirname "$db")"
        while IFS= read -r -d '' f; do
            rm -f "$f" && echo "  removed $f" || echo "  failed to remove $f"
            JFIXED=$((JFIXED + 1))
        done < <(find "$d" -maxdepth 1 \
            \( -name '.fslckout-journal' -o -name '.fslckout-wal' -o -name '.fslckout-shm' \
            -o -name '*.fossil-journal' -o -name '*.fossil-wal' -o -name '*.fossil-shm' \) -print0 2>/dev/null || true)
    done
    FIXED=$((FIXED + JFIXED))
    if [ "$JFIXED" -eq 0 ]; then
        echo '  no stale journal files found'
    fi

    # 3. Verify every known checkout is now writable
    echo ''
    ALL_OK=1
    for db in "${FILES[@]}"; do
        # Only .fslckout dirs are checkout roots; the $REPO file is not.
        case "$db" in
            */.fslckout)
                d="$(dirname "$db")"
                if (cd "$d" && fossil status >/dev/null 2>&1); then
                    :
                else
                    echo "Warning: $d may still be locked." >&2
                    ALL_OK=0
                fi
                ;;
        esac
    done
    if [ "$ALL_OK" -eq 1 ]; then
        echo "Lock cleared successfully ($FIXED fix(es) applied)."
    else
        echo 'Warning: one or more checkouts may still be locked. Check processes manually.' >&2
        exit 1
    fi

# Mirror trunk history to GitHub (one-way, force push)
sync-github:
   #!/bin/bash
   set -euo pipefail

   FOSSIL_REPO="/mnt/zed/repos/jinn/jinn.fossil"
   MIRROR_DIR="/mnt/zed/repos/jinn/.github-mirror"
   GITHUB_REMOTE="git@github.com:jayson-lennon/jinn.git"

   fossil git export "$MIRROR_DIR" \
       --repository "$FOSSIL_REPO" \
       --mainbranch trunk \
       --autopush "$GITHUB_REMOTE"

   # Prune mirror branches whose fossil branch is closed. fossil's git export
   # never deletes refs (verified), so closed branches accumulate on GitHub
   # forever. Deletion here is mirror-only and does NOT delete commits — all
   # history stays reachable via trunk on GitHub. A branch still open in
   # fossil would be re-created by the next export, so only closed branches
   # qualify. "Closed" = fossil's own rule (src/branch.c): the branch's
   # current leaf carries the 'closed' tag — matches `fossil branch ls`.
   echo '==> Pruning mirror branches closed in fossil'
   git -C "$MIRROR_DIR" for-each-ref --format='%(refname:short)' refs/heads |
   while IFS= read -r branch; do
       # the mirror's main branch IS fossil's trunk (sync-github exports with
       # --mainbranch trunk, which becomes 'main' in git); never prune it.
       # (fossil-side the sql would return empty for 'main' anyway, but make
       # the intent explicit.)
       [ "$branch" = "main" ] && continue
       # "Closed" uses fossil's own createBrlistQuery semantics (src/branch.c):
       # the branch's newest commit (max mtime) carries the 'closed' tag. The
       # bare bx.rid inside the aggregate is NOT a bug — SQLite binds bare
       # columns to the single max(mtime) row, which is exactly what fossil's
       # `branch ls` relies on. Results verified 1:1 against `fossil branch ls`.
       closed=$(fossil sql -R "$FOSSIL_REPO" "
           SELECT count(*) FROM (
             SELECT bx.value AS name,
                    max(event.mtime) AS mtime,
                    EXISTS(SELECT 1 FROM tagxref tx
                           WHERE tx.rid=bx.rid
                             AND tx.tagid=(SELECT tagid FROM tag WHERE tagname='closed')
                             AND tx.tagtype>0) AS isclosed
             FROM tagxref bx, tag, event
             WHERE bx.tagid=tag.tagid AND tag.tagname='branch' AND bx.tagtype>0
               AND event.objid=bx.rid AND bx.value='$branch'
             GROUP BY bx.value) WHERE isclosed=1;" 2>/dev/null || true)
       if [ "$closed" = "1" ]; then
           echo "    deleting closed branch: $branch"
           git -C "$MIRROR_DIR" branch -D "$branch" >/dev/null
       fi
   done

   # Publish the pruned mirror. `--mirror` is what makes deletions propagate;
   # fossil's own autopush would leave the deleted refs alive on GitHub.
   echo '==> Pushing mirror (with deletions)'
   git -C "$MIRROR_DIR" push --mirror "$GITHUB_REMOTE"

# ---------------------------------------------------------------------------
# GitHub PR intake (fossil-primary, patch-based — no git↔fossil import).
#
#   just gh-setup              one-time: teach `gh` which repo to talk to
#   just gh-pr-list [STATE]    browse PRs (default open)
#   just gh-pr-view N          PR metadata in the terminal
#   just gh-pr-fetch N         download patch + metadata, extract the author
#   just gh-pr-apply N         apply the patch; conflicts become conflict markers
#   ...review, `just check`, `just test`...
#   just gh-pr-land N          commit, attributed to the contributor
#   just sync-github           mirror the landed commit to GitHub
#   just gh-pr-close N [MSG]   close the PR on GitHub with a note
#
# Per-PR artifacts live in target/gh-pr/N/ (ignored via the `target` glob).
# ---------------------------------------------------------------------------
export GH_REPO := "jayson-lennon/jinn"
GH_PR_STATE_DIR := "target/gh-pr"

# One-time: export GH_REPO so gh works outside git (gh repo set-default refuses fossil-only workspaces)
gh-setup:
    @echo "GH_REPO={{GH_REPO}} (exported by the justfile; no setup needed)"
    @GH_REPO={{GH_REPO}} gh auth status && echo "gh is ready"

gh-pr-list STATE="open":
    gh pr list --state {{STATE}} --limit 30 \
        --json number,title,author,headRefName \
        --jq '.[] | "#\(.number)  \(.author.login)  [\(.headRefName)]  \(.title)"'

gh-pr-view N:
    gh pr view {{N}} \
        --json number,title,url,author,headRefName,headRefOid,additions,deletions,changedFiles \
        --jq '"#\(.number) \(.title)", "  by \(.author.login)  \(.headRefName)@\(.headRefOid[0:10])", "  +\(.additions) -\(.deletions) across \(.changedFiles) file(s)", "  \(.url)"'

# Fetch PR patch + extract contributor identity from the first commit's "From:" header
gh-pr-fetch N:
    #!/usr/bin/env bash
    set -euo pipefail
    dir="{{GH_PR_STATE_DIR}}/{{N}}"
    mkdir -p "$dir"

    echo '==> Fetching PR metadata'
    gh pr view {{N}} \
        --json number,title,url,author,headRefName,headRepositoryOwner,isCrossRepository \
        > "$dir/meta.json"

    echo '==> Fetching patch'
    # NOTE: `gh pr diff` can exit 0 while writing an error string into the
    # file on some failure modes (observed: HTTP 422 → empty file). Guard
    # with a non-empty check too, not just the exit code.
    gh pr diff {{N}} --patch > "$dir/pr.patch"
    if [ ! -s "$dir/pr.patch" ] || ! grep -q '^From ' "$dir/pr.patch"; then
        echo "ERROR: no patch content for PR {{N}} (diff unavailable?)" >&2
        exit 1
    fi

    commits=$(grep -c '^From ' "$dir/pr.patch" || true)
    ident=$(awk 'NR>1 && /^From: */{ sub(/^From: */,""); print; exit }' "$dir/pr.patch")
    if [ -z "$ident" ]; then
        echo 'ERROR: no "From:" header in patch; cannot extract author identity.' >&2
        exit 1
    fi
    printf '%s\n' "$ident" > "$dir/identity"

    echo "    commits : $commits"
    echo "    author  : $ident"
    if [ "$commits" -gt 1 ]; then
        echo '    note    : multi-commit PR; identity taken from the FIRST commit' >&2
    fi
    echo "==> Saved: $dir/{meta.json,pr.patch,identity}"

# Apply the fetched patch to the working tree. Refuses a dirty tree.
# Clean apply: patch goes straight in. Conflicting hunks are written into
# the files as `<<<<<<<`/`=======`/`>>>>>>>` markers (--merge) — resolve
# them by hand or hand them to an agent (they're greppable), then land.
gh-pr-apply N:
    #!/usr/bin/env bash
    set -euo pipefail
    dir="{{GH_PR_STATE_DIR}}/{{N}}"
    [ -f "$dir/pr.patch" ] || {
        echo "ERROR: no fetched patch for PR {{N}}; run: just gh-pr-fetch {{N}}" >&2
        exit 1
    }
    if [ -n "$(fossil changes --differ)" ]; then
        echo 'ERROR: working tree has uncommitted changes; commit or revert first.' >&2
        exit 1
    fi
    branch=$(fossil branch current)
    if [ "$branch" != "trunk" ]; then
        echo "Note: applying on branch '$branch', not trunk" >&2
    fi

    echo '==> Probing with dry run'
    if patch -p1 --dry-run < "$dir/pr.patch" >/dev/null 2>&1; then
        echo '==> Clean apply'
        patch -p1 < "$dir/pr.patch"
    else
        echo '==> Conflicts detected: writing conflict markers into the tree'
        patch -p1 --merge < "$dir/pr.patch" || true
        # drop patch's .orig backups; the conflicted files themselves carry
        # the pre-image in the `<<<<<<<` block, so .orig is redundant noise
        find . -name '*.orig' -not -path './.fossil/*' -not -path './target/*' -delete
        echo
        echo '    Conflicted spots (resolve every one, then delete its markers):'
        grep -rn '^<<<<<<<' --exclude-dir=.fossil --exclude-dir=target . || true
        echo
        echo '    After resolving:'
        echo '      just check && just test'
        echo "      just gh-pr-land {{N}}"
    fi
    fossil addremove --dotfiles
    echo "==> Applied. Then: just gh-pr-land {{N}}"

# Commit the applied patch attributed to the contributor (auto-provisions a fossil user).
# Refuses to run while unresolved conflict markers remain in the tree.
gh-pr-land N:
    #!/usr/bin/env bash
    set -euo pipefail
    dir="{{GH_PR_STATE_DIR}}/{{N}}"
    [ -f "$dir/pr.patch" ] || {
        echo "ERROR: no fetched patch for PR {{N}}; run: just gh-pr-fetch {{N}}" >&2
        exit 1
    }
    if [ -z "$(fossil changes --differ)" ]; then
        echo 'ERROR: nothing to commit; apply the patch first: just gh-pr-apply {{N}}' >&2
        exit 1
    fi

    if grep -rn '^<<<<<<<' --exclude-dir=.fossil --exclude-dir=target . >/dev/null 2>&1; then
        echo 'ERROR: unresolved conflict markers remain:' >&2
        grep -rn '^<<<<<<<' --exclude-dir=.fossil --exclude-dir=target . | head -10 >&2
        echo 'Resolve them (or hand them to an agent), then re-run.' >&2
        exit 1
    fi

    ident=$(<"$dir/identity")
    login=$(jq -r '.author.login' "$dir/meta.json")
    url=$(jq -r '.url' "$dir/meta.json")
    email=${ident#*<}; email=${email%>*}

    if fossil user list | awk '{print $1}' | grep -qx "$login"; then
        echo "==> fossil user '$login' already exists (contact not modified)"
    else
        # --pw '': contributors never log into the fossil repo; their account
        # exists purely to carry attribution (contact info + --user-override).
        fossil user new "$login" "$ident" --pw "" >/dev/null
        echo "==> created fossil user '$login' ($ident)"
    fi

    # --user-override already records the contributor as the check-in user
    # (rendered as "user: <login>" in the timeline and as git author in the
    # mirror), so the comment doesn't repeat the "by <name>" attribution.
    msg=$(printf 'Apply PR #{{N}} from %s\n\nPicked from %s\n' \
        "$ident" "$url")
    fossil addremove --dotfiles
    fossil commit -m "$msg" --user-override "$login" --no-verify-comment

    hash=$(fossil info | awk '/^checkout:/{ print substr($2, 1, 12); exit }')
    echo
    echo "==> Landed as $hash (user: $login <$email>)"
    echo '    next: just check && just test   (if not already done)'
    echo '    then: just sync-github'
    echo "    then: just gh-pr-close {{N}} 'Landed in Fossil as $hash; mirrored shortly.'"

# Opt-in: close the PR on GitHub (pass COMMENT for a ready-made message from gh-pr-land output)
gh-pr-close N COMMENT="":
    #!/usr/bin/env bash
    set -euo pipefail
    args=()
    if [ -n "{{COMMENT}}" ]; then
        args+=(--comment "{{COMMENT}}")
    fi
    gh pr close {{N}} "${args[@]}"

# Prune merged branches from the git mirror (preview default; PRUNE_ALL=1 also deletes unmerged)
mirror-prune MODE="preview" PRUNE_ALL="":
    #!/usr/bin/env bash
    set -euo pipefail
    MIRROR_DIR="/mnt/zed/repos/jinn/.github-mirror"
    main_branch="trunk"   # sync-github exports with --mainbranch trunk
    cd "$MIRROR_DIR"

    merged=()
    unmerged=()
    while IFS= read -r b; do
        [ "$b" = "$main_branch" ] && continue
        if git merge-base --is-ancestor "$b" "$main_branch" 2>/dev/null; then
            merged+=("$b")
        else
            unmerged+=("$b")
        fi
    done < <(git for-each-ref --format='%(refname:short)' refs/heads)
    n_merged=${#merged[@]}
    n_unmerged=${#unmerged[@]}
    n_total=$(( n_merged + n_unmerged ))

    echo "mirror branches: total $n_total = $n_merged merged + $n_unmerged unmerged (into '$main_branch')"

    if [ "$n_unmerged" -gt 0 ]; then
        echo '-- unmerged branches (NOT touched unless PRUNE_ALL=1):'
        printf '   %s\n' "${unmerged[@]}" | head -40
        [ "${#unmerged[@]}" -gt 40 ] && echo "   ... and $(( ${#unmerged[@]} - 40 )) more"
    fi

    case "{{MODE}}" in
        preview)
            echo "==> preview only; run: just mirror-prune apply"
            ;;
        apply)
            n=0
            for b in "${merged[@]}"; do
                git branch -d "$b" && n=$((n+1))
            done
            if [ "{{PRUNE_ALL}}" = "1" ]; then
                echo "==> PRUNE_ALL=1: deleting unmerged branches too"
                for b in "${unmerged[@]}"; do
                    git branch -D "$b" && n=$((n+1))
                done
            fi
            echo "==> deleted $n branch(es) from the mirror"
            echo "    next: just sync-github   (pushes deletions to GitHub)"
            ;;
        *)
            echo "ERROR: MODE must be 'preview' or 'apply'" >&2
            exit 1
            ;;
    esac

# Bump version (major/minor/patch), commit, and tag in Fossil
bump LEVEL:
    #!/usr/bin/env bash
    set -euo pipefail

    # --- Validate input ---
    case "{{LEVEL}}" in
        major|minor|patch) ;;
        *) echo "Usage: just bump <major|minor|patch>" >&2; exit 1 ;;
    esac

    # --- Pre-flight: must be on trunk ---
    BRANCH=$(fossil branch current)
    if [ "$BRANCH" != "trunk" ]; then
        echo "Error: must be on trunk (currently on '$BRANCH')" >&2
        exit 1
    fi

    # --- Pre-flight: working tree must be clean ---
    if [ -n "$(fossil changes --differ)" ]; then
        echo "Error: working tree has uncommitted changes" >&2
        exit 1
    fi

    # --- Compute new version ---
    CURRENT=$(sed -n '/^\[workspace\.package\]/,/^\[/{s/^version = "\(.*\)"/\1/p}' Cargo.toml)
    NEW=$(rust-script scripts/bump-version.rs "$CURRENT" "{{LEVEL}}")

    # --- Resolve tag conflicts ---
    CANDIDATE="$NEW"
    ATTEMPTS=0
    while fossil tag list | grep -qx "v${CANDIDATE}"; do
        echo "Tag v${CANDIDATE} already exists, skipping..."
        ATTEMPTS=$((ATTEMPTS + 1))
        if [ "$ATTEMPTS" -ge 100 ]; then
            echo "Error: too many tag collisions, aborting" >&2
            exit 1
        fi
        CANDIDATE=$(rust-script scripts/bump-version.rs "$CANDIDATE" "patch")
    done

    # --- Update files ---
    sed -i "/^\[workspace\.package\]/,/^\[/{s/^version = \".*\"/version = \"$CANDIDATE\"/}" Cargo.toml

    # --- Regenerate Cargo.lock for the new workspace version ---
    cargo update --workspace

    # --- Commit ---
    fossil commit -m "Bump version to ${CANDIDATE}"

    # --- Tag the current checkout ---
    fossil tag add "v${CANDIDATE}" current

    echo "Bumped to ${CANDIDATE}, committed and tagged as v${CANDIDATE}"


# Symlink ./target to /dev/shm/<parent-dir> for faster builds
shm-target:
    #!/bin/bash
    set -euo pipefail

    NAME="$(basename '{{justfile_directory()}}')"
    SHM_DIR="/dev/shm/${NAME}"

    if [ -L target ]; then
        CURRENT="$(readlink target)"
        if [ "$CURRENT" = "$SHM_DIR" ]; then
            echo "target already points to $SHM_DIR"
            exit 0
        fi
        echo "target is a symlink to $CURRENT — removing"
        rm target
    fi

    if [ -d target ]; then
        echo "target/ exists as a directory — removing"
        rm -rf target
    fi

    mkdir -p "$SHM_DIR"
    ln -s "$SHM_DIR" target
    echo "target -> $SHM_DIR"

# Remove the /dev/shm/<parent-dir> symlink and directory
unshm-target:
    #!/bin/bash
    set -euo pipefail

    NAME="$(basename '{{justfile_directory()}}')"
    SHM_DIR="/dev/shm/${NAME}"

    if [ -L target ]; then
        CURRENT="$(readlink target)"
        if [ "$CURRENT" = "$SHM_DIR" ]; then
            rm target
            echo "removed target symlink"
        fi
    fi

    if [ -d "$SHM_DIR" ]; then
        rm -rf "$SHM_DIR"
        echo "removed $SHM_DIR"
    else
        echo "$SHM_DIR does not exist"
    fi

# Build the Arch package in ./build (isolated src/pkg scratch dirs; run from repo root)
pkg:
    @mkdir -p build
    @BUILDDIR="$(pwd)/build" PKGDEST="$(pwd)/build" makepkg -f

# --- Release (cargo-binstall) -----------------------------------------------
#
# `jinn` is distributed as a prebuilt binary attached to GitHub releases.
# `cargo-binstall` downloads it (see [package.metadata.binstall] in Cargo.toml).
# Source tarballs are auto-generated by GitHub per tag — no work needed.
#
# Prerequisite: install the GitHub CLI and authenticate once:
#     https://cli.github.com/   then   gh auth login

# Build the release binary and package it into a cargo-binstall tarball.
# TARGET defaults to the native linux release target; pass x86_64-pc-windows-gnu
# to cross-build the Windows tarball (needs mingw-w64 + the rustup target).
build-release-tarball TARGET="x86_64-unknown-linux-gnu":
    #!/usr/bin/env bash
    set -euo pipefail

    VERSION=$(sed -n '/^\[workspace\.package\]/,/^[\[]/{s/^version = "\(.*\)"/\1/p}' Cargo.toml)
    TARGET="{{TARGET}}"
    STAGE="jinn-${TARGET}-v${VERSION}"
    TARBALL="${STAGE}.tgz"

    # --- Prerequisites for cross targets ---
    case "${TARGET}" in
        *windows*)
            if ! command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
                echo "Error: x86_64-w64-mingw32-gcc (mingw-w64) is not on PATH." >&2
                echo "  Arch: pacman -S mingw-w64-gcc   Debian/Ubuntu: apt install gcc-mingw-w64-x86-64" >&2
                exit 1
            fi
            ;;
    esac
    if ! rustup target list --installed 2>/dev/null | grep -qx "${TARGET}"; then
        echo "Error: rustup target '${TARGET}' is not installed." >&2
        echo "  Run: rustup target add ${TARGET}" >&2
        exit 1
    fi

    # Binary name: cargo emits foo.exe for windows targets, foo otherwise.
    BIN="jinn"
    case "${TARGET}" in
        *windows*) BIN="jinn.exe" ;;
    esac

    echo "==> Building release binary (target ${TARGET})"
    cargo build --release --target "${TARGET}"

    echo "==> Packaging ${TARBALL}"
    STAGE_DIR="$(mktemp -d)"
    mkdir -p "${STAGE_DIR}/${STAGE}"
    cp "target/${TARGET}/release/${BIN}" "${STAGE_DIR}/${STAGE}/${BIN}"
    tar -czf "${TARBALL}" -C "${STAGE_DIR}" "${STAGE}"
    rm -rf "${STAGE_DIR}"

    echo "==> Created ${TARBALL}"

# Release to GitHub + smoke-test cargo-binstall (after `just bump`; needs gh auth + cargo-binstall)
release TAG:
    #!/usr/bin/env bash
    set -euo pipefail

    REPO="jayson-lennon/jinn"
    LINUX_TARGET="x86_64-unknown-linux-gnu"
    WINDOWS_TARGET="x86_64-pc-windows-gnu"
    VERSION=$(sed -n '/^\[workspace\.package\]/,/^[\[]/{s/^version = "\(.*\)"/\1/p}' Cargo.toml)
    if [ "{{TAG}}" != "v${VERSION}" ]; then
        echo "Error: tag '{{TAG}}' does not match Cargo.toml version 'v${VERSION}'." >&2
        echo "  Run 'just bump <major|minor|patch>' first, then 'just release v${VERSION}'." >&2
        exit 1
    fi

    # --- Pre-flight: gh must be installed ---
    if ! command -v gh >/dev/null 2>&1; then
        echo "Error: GitHub CLI (gh) is not installed." >&2
        echo "  Install from https://cli.github.com/ then run: gh auth login" >&2
        exit 1
    fi

    # --- Pre-flight: gh must be authenticated ---
    if ! gh auth status >/dev/null 2>&1; then
        echo "Error: gh is not authenticated." >&2
        echo "  Run: gh auth login" >&2
        exit 1
    fi

    # --- 1. Mirror trunk (and tags) to GitHub ---
    echo '==> Mirroring trunk to GitHub...'
    just sync-github

    # --- 2. Refresh bundled plugin payloads (embedded into the binary) ---
    # MUST run before both target builds: the wasm payloads are compiled in.
    just refresh-plugins

    # --- 3. Build the cargo-binstall tarballs (linux + windows) ---
    just build-release-tarball "${LINUX_TARGET}"
    just build-release-tarball "${WINDOWS_TARGET}"

    TARBALL_LINUX="jinn-x86_64-unknown-linux-gnu-v${VERSION}.tgz"
    TARBALL_WINDOWS="jinn-x86_64-pc-windows-gnu-v${VERSION}.tgz"

    # --- 4. Create the release if it doesn't exist, else upload ---
    if gh release view "{{TAG}}" --repo "${REPO}" >/dev/null 2>&1; then
        echo "==> Uploading tarballs to existing release {{TAG}}"
        gh release upload "{{TAG}}" "${TARBALL_LINUX}" "${TARBALL_WINDOWS}" --repo "${REPO}" --clobber
    else
        echo "==> Creating release {{TAG}} and uploading tarballs"
        gh release create "{{TAG}}" "${TARBALL_LINUX}" "${TARBALL_WINDOWS}" --repo "${REPO}" --generate-notes
    fi

    # --- 5. Verify the uploaded artifacts locally ---
    # The [package.metadata.binstall] templates resolve
    # jinn-<target>-v<version>.tgz -> jinn-<target>-v<version>/<bin><binary-ext>,
    # so the tarball member path IS what binstall looks up. Checking the
    # members (and running each binary) verifies the template against the
    # assets we just built — no network round-trip needed.
    echo '==> Verifying tarball layouts (binstall template: {name}-{target}-v{version}/{bin}{binary-ext})'
    [ "$(tar -tzf "${TARBALL_LINUX}" | grep -cx "jinn-${LINUX_TARGET}-v${VERSION}/jinn")" -eq 1 ] \
        || { echo "Error: ${TARBALL_LINUX} does not contain jinn-${LINUX_TARGET}-v${VERSION}/jinn" >&2; exit 1; }
    [ "$(tar -tzf "${TARBALL_WINDOWS}" | grep -cx "jinn-${WINDOWS_TARGET}-v${VERSION}/jinn.exe")" -eq 1 ] \
        || { echo "Error: ${TARBALL_WINDOWS} does not contain jinn-${WINDOWS_TARGET}-v${VERSION}/jinn.exe" >&2; exit 1; }

    echo "==> Linux binary reports: $(./target/${LINUX_TARGET}/release/jinn --version)"
    if command -v wine >/dev/null 2>&1; then
        echo "==> Windows binary reports: $(WINEDEBUG=-all wine ./target/${WINDOWS_TARGET}/release/jinn.exe --version 2>/dev/null)"
    else
        echo '==> wine not on PATH; skipping windows binary run'
    fi

    echo "==> Done. https://github.com/${REPO}/releases/tag/{{TAG}}"
