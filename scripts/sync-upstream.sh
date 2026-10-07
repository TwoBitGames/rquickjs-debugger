#!/usr/bin/env bash

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FORK_DIR="crates/rquickjs-sys"
UPSTREAM_BRANCH="upstream"
FORK_OWNED=(Cargo.toml)
REGISTRY_ARTIFACTS=(.cargo-ok .cargo_vcs_info.json .gitignore Cargo.lock Cargo.toml.orig Makefile)

usage() {
  cat >&2 <<USAGE
usage:
  $0 init         record the current upstream version as the merge base (once)
  $0 <version>    import rquickjs-sys <version> onto '$UPSTREAM_BRANCH' and merge it
USAGE
  exit 2
}

fork_version() {
  sed -n 's/^version = "\(.*\)"/\1/p' "$REPO_ROOT/$FORK_DIR/Cargo.toml" | head -1
}

upstream_dir() {
  local version="$1" dir
  dir="$(ls -d "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/"rquickjs-sys-$version" 2>/dev/null | head -1 || true)"
  if [ -z "$dir" ]; then
    echo "fetching rquickjs-sys $version" >&2
    local tmp
    tmp="$(mktemp -d)"
    mkdir -p "$tmp/src" && : > "$tmp/src/lib.rs"
    printf '[package]\nname = "fetch"\nversion = "0.0.0"\nedition = "2021"\n\n[dependencies]\nrquickjs-sys = "=%s"\n' "$version" > "$tmp/Cargo.toml"
    (cd "$tmp" && cargo fetch --quiet)
    rm -rf "$tmp"
    dir="$(ls -d "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/"rquickjs-sys-$version" 2>/dev/null | head -1 || true)"
  fi
  if [ -z "$dir" ]; then
    echo "rquickjs-sys $version is not available" >&2
    exit 1
  fi
  echo "$dir"
}

require_clean() {
  if [ -n "$(git -C "$REPO_ROOT" status --porcelain)" ]; then
    echo "the working tree has uncommitted changes; commit or stash them first" >&2
    exit 1
  fi
}

branch_exists() {
  git -C "$REPO_ROOT" show-ref --verify --quiet "refs/heads/$UPSTREAM_BRANCH"
}

snapshot() {
  local version="$1" source wt
  source="$(upstream_dir "$version")"
  wt="$(mktemp -d)"
  if branch_exists; then
    git -C "$REPO_ROOT" worktree add --quiet "$wt" "$UPSTREAM_BRANCH"
  else
    git -C "$REPO_ROOT" worktree add --quiet --detach "$wt" HEAD
    git -C "$wt" checkout --quiet --orphan "$UPSTREAM_BRANCH"
    git -C "$wt" rm -rfq .
  fi
  rm -rf "$wt/$FORK_DIR"
  mkdir -p "$wt/$FORK_DIR"
  cp -R "$source/." "$wt/$FORK_DIR/"
  local file
  for file in "${REGISTRY_ARTIFACTS[@]}" "${FORK_OWNED[@]}"; do
    rm -rf "$wt/$FORK_DIR/$file"
  done
  git -C "$wt" add -A
  git -C "$wt" commit --quiet --message "rquickjs-sys $version"
  git -C "$REPO_ROOT" worktree remove --force "$wt"
  echo "recorded rquickjs-sys $version on '$UPSTREAM_BRANCH'"
}

[ $# -eq 1 ] || usage
require_clean

if [ "$1" = "init" ]; then
  if branch_exists; then
    echo "'$UPSTREAM_BRANCH' already exists" >&2
    exit 1
  fi
  version="$(fork_version)"
  snapshot "$version"
  git -C "$REPO_ROOT" merge --quiet --strategy ours --allow-unrelated-histories --no-edit \
    --message "Record rquickjs-sys $version as the upstream merge base" "$UPSTREAM_BRANCH"
  echo "done; future upgrades: $0 <version>"
  exit 0
fi

new_version="$1"
old_version="$(fork_version)"
if ! branch_exists; then
  echo "'$UPSTREAM_BRANCH' does not exist; run '$0 init' first" >&2
  exit 1
fi
if [ "$new_version" = "$old_version" ]; then
  echo "already on rquickjs-sys $old_version" >&2
  exit 1
fi

snapshot "$new_version"

status=0
git -C "$REPO_ROOT" merge --no-commit --no-ff "$UPSTREAM_BRANCH" || status=$?

sed -i "s/^version = \"$old_version\"/version = \"$new_version\"/" "$REPO_ROOT/$FORK_DIR/Cargo.toml"
sed -i "s/^rquickjs = \"$old_version\"/rquickjs = \"$new_version\"/" "$REPO_ROOT/Cargo.toml"
git -C "$REPO_ROOT" add "$FORK_DIR/Cargo.toml" Cargo.toml

echo
if [ $status -ne 0 ]; then
  echo "merge conflicts; resolve them, then 'cargo test --workspace' and 'git commit'"
else
  echo "merged cleanly; 'cargo test --workspace', then 'git commit'"
fi
exit $status
