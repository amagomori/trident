#!/usr/bin/env bash
# Commits files into a package repository and pushes it, e.g. the Homebrew tap or the AUR.
# Usage: push_package.sh REMOTE DIR MESSAGE FILE...  (files land in DIR inside the repository)
# BRANCH names the branch to push (the AUR only takes master); the clone's own branch otherwise.
set -euo pipefail
remote=$1 dir=$2 message=$3
shift 3
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
git clone -q "$remote" "$work/repo"
mkdir -p "$work/repo/$dir"
cp "$@" "$work/repo/$dir/"
cd "$work/repo"
if [ -n "${BRANCH:-}" ]; then git checkout -q -B "$BRANCH"; fi
git add -A
if git diff --cached --quiet; then
  echo "$remote is already up to date"
  exit 0
fi
git -c user.name="Trident release" -c user.email="noreply@github.com" commit -q -m "$message"
git push -q origin HEAD
echo "pushed $message to $remote"
