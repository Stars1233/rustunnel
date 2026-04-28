#!/usr/bin/env bash
# bump-version.sh — update version in all crate Cargo.toml files
#
# Usage:
#   ./scripts/bump-version.sh                 # auto-bump patch (with rollover)
#   ./scripts/bump-version.sh patch           # auto-bump patch (with rollover)
#   ./scripts/bump-version.sh minor           # bump minor, reset patch
#   ./scripts/bump-version.sh major           # bump major, reset minor and patch
#   ./scripts/bump-version.sh 1.2.3           # set explicit version
#
# Rollover thresholds (configurable via env):
#   MAX_PATCH (default 99) — when bumping patch, if new patch > MAX_PATCH, rollover to minor
#   MAX_MINOR (default 99) — when bumping minor, if new minor > MAX_MINOR, rollover to major
#
# Examples (with defaults MAX_PATCH=99, MAX_MINOR=99):
#   0.5.1   → patch → 0.5.2
#   0.5.99  → patch → 0.6.0   (patch rollover)
#   0.99.99 → patch → 1.0.0   (cascading rollover)
#   1.2.3   → minor → 1.3.0

set -euo pipefail

# ── configurable thresholds ────────────────────────────────────────────────────

MAX_PATCH="${MAX_PATCH:-99}"
MAX_MINOR="${MAX_MINOR:-99}"

if ! [[ "$MAX_PATCH" =~ ^[0-9]+$ ]] || (( MAX_PATCH < 0 )); then
    echo "Error: MAX_PATCH must be a non-negative integer (got: $MAX_PATCH)"
    exit 1
fi
if ! [[ "$MAX_MINOR" =~ ^[0-9]+$ ]] || (( MAX_MINOR < 0 )); then
    echo "Error: MAX_MINOR must be a non-negative integer (got: $MAX_MINOR)"
    exit 1
fi

# ── parse argument ─────────────────────────────────────────────────────────────

if [[ $# -gt 1 ]]; then
    echo "Usage: $0 [patch|minor|major|<X.Y.Z>]"
    exit 1
fi

ARG="${1:-patch}"

# ── locate repo root ───────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

CRATES=(
    "crates/rustunnel-client/Cargo.toml"
    "crates/rustunnel-server/Cargo.toml"
    "crates/rustunnel-mcp/Cargo.toml"
    "crates/rustunnel-protocol/Cargo.toml"
)

# ── read current version from the first crate ─────────────────────────────────

FIRST_FILE="$REPO_ROOT/${CRATES[0]}"
if [[ ! -f "$FIRST_FILE" ]]; then
    echo "Error: $FIRST_FILE not found"
    exit 1
fi
OLD_VERSION=$(grep '^version' "$FIRST_FILE" | head -1 | sed 's/version = "\(.*\)"/\1/')

if ! [[ "$OLD_VERSION" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
    echo "Error: could not parse current version '$OLD_VERSION' from $FIRST_FILE"
    exit 1
fi
CUR_MAJOR="${BASH_REMATCH[1]}"
CUR_MINOR="${BASH_REMATCH[2]}"
CUR_PATCH="${BASH_REMATCH[3]}"

# ── compute new version ────────────────────────────────────────────────────────

case "$ARG" in
    patch)
        NEW_MAJOR="$CUR_MAJOR"
        NEW_MINOR="$CUR_MINOR"
        NEW_PATCH=$((CUR_PATCH + 1))
        if (( NEW_PATCH > MAX_PATCH )); then
            NEW_PATCH=0
            NEW_MINOR=$((CUR_MINOR + 1))
            if (( NEW_MINOR > MAX_MINOR )); then
                NEW_MINOR=0
                NEW_MAJOR=$((CUR_MAJOR + 1))
            fi
        fi
        NEW_VERSION="${NEW_MAJOR}.${NEW_MINOR}.${NEW_PATCH}"
        ;;
    minor)
        NEW_MAJOR="$CUR_MAJOR"
        NEW_MINOR=$((CUR_MINOR + 1))
        NEW_PATCH=0
        if (( NEW_MINOR > MAX_MINOR )); then
            NEW_MINOR=0
            NEW_MAJOR=$((CUR_MAJOR + 1))
        fi
        NEW_VERSION="${NEW_MAJOR}.${NEW_MINOR}.${NEW_PATCH}"
        ;;
    major)
        NEW_MAJOR=$((CUR_MAJOR + 1))
        NEW_MINOR=0
        NEW_PATCH=0
        NEW_VERSION="${NEW_MAJOR}.${NEW_MINOR}.${NEW_PATCH}"
        ;;
    *)
        if [[ "$ARG" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
            NEW_VERSION="$ARG"
        else
            echo "Error: argument must be 'patch', 'minor', 'major', or a semver (e.g. 1.2.3); got: $ARG"
            exit 1
        fi
        ;;
esac

# ── show plan ──────────────────────────────────────────────────────────────────

echo "Current version: $OLD_VERSION"
echo "New version:     $NEW_VERSION  (bump: $ARG; MAX_PATCH=$MAX_PATCH, MAX_MINOR=$MAX_MINOR)"
echo ""

for CRATE in "${CRATES[@]}"; do
    FILE="$REPO_ROOT/$CRATE"
    if [[ ! -f "$FILE" ]]; then
        echo "Error: $FILE not found"
        exit 1
    fi
    CURRENT=$(grep '^version' "$FILE" | head -1 | sed 's/version = "\(.*\)"/\1/')
    echo "  $CRATE  ($CURRENT → $NEW_VERSION)"
done

echo ""

# ── confirm ────────────────────────────────────────────────────────────────────

read -r -p "Continue? [y/N] " CONFIRM
if [[ ! "$CONFIRM" =~ ^[Yy]$ ]]; then
    echo "Aborted."
    exit 0
fi

echo ""

# ── update files ───────────────────────────────────────────────────────────────

for CRATE in "${CRATES[@]}"; do
    FILE="$REPO_ROOT/$CRATE"
    # Replace only the first occurrence of ^version = "..." in each file
    # (avoids touching [dependencies] version fields)
    # awk is used for cross-platform compatibility (BSD sed on macOS lacks 0, addr)
    awk -v ver="$NEW_VERSION" '
        /^version = / && done == 0 { sub(/"[^"]*"/, "\"" ver "\""); done=1 }
        { print }
    ' "$FILE" > "$FILE.tmp" && mv "$FILE.tmp" "$FILE"
    echo "  Updated $CRATE"
done

echo ""

# ── rebuild workspace ──────────────────────────────────────────────────────────

echo "Running cargo build --workspace …"
echo ""
cd "$REPO_ROOT"
cargo build --workspace

echo ""
echo "Done. All crates are now at version $NEW_VERSION."
echo ""
echo "Next steps:"
echo "  git add -A"
echo "  git commit -m \"chore: bump version to $NEW_VERSION\""
echo "  git tag v$NEW_VERSION"
echo "  git push && git push --tags"
