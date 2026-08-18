#!/usr/bin/env bash
#
# Cuts a release: bumps the version, tags it, builds the signed universal
# binary, signs the artifact with the project release key, and publishes a
# GitHub release. `swivel update` installs from what this publishes.
#
#   ./scripts/release.sh 0.2.0
#
# Run it on main with a clean tree. It refuses anything else.

set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="${1:-}"
if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "usage: ./scripts/release.sh <major.minor.patch>" >&2
  exit 1
fi
TAG="v$VERSION"

if [[ "$(git branch --show-current)" != "main" ]]; then
  echo "releases are cut from main." >&2
  exit 1
fi
if [[ -n "$(git status --porcelain)" ]]; then
  echo "the tree is not clean. Commit or stash first." >&2
  exit 1
fi
if git rev-parse "$TAG" >/dev/null 2>&1; then
  echo "$TAG already exists." >&2
  exit 1
fi

echo "==> checking the build"
cargo clippy --all-targets -- -D warnings
cargo test

echo "==> setting the version to $VERSION"
sed -i '' "s/^version = \".*\"/version = \"$VERSION\"/" Cargo.toml
cargo check --quiet   # refresh Cargo.lock
git add Cargo.toml Cargo.lock
git commit -m "release: $TAG"
git tag "$TAG"

echo "==> building the universal binary"
./scripts/build-release.sh

echo "==> signing the artifact with the release key"
# The freshly built binary signs the file. The command refuses a key that does
# not match the public key compiled into this build, so a release that clients
# would reject cannot be uploaded.
./swivel release-sign ./swivel

echo "==> publishing"
git push
git push origin "$TAG"
gh release create "$TAG" ./swivel ./swivel.sig \
  --title "$TAG" \
  --generate-notes

echo
echo "==> $TAG is published. Anyone on an older version runs: swivel update"
