#!/bin/bash
# Check that versions are in sync across files

set -e

CARGO_VERSION=$(grep '^version = ' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
NPM_VERSION=$(node -p "require('./npm/package.json').version")

echo "Cargo.toml version: $CARGO_VERSION"
echo "npm/package.json version: $NPM_VERSION"

if [ "$CARGO_VERSION" != "$NPM_VERSION" ]; then
    echo "Error: Version mismatch!"
    exit 1
fi

echo "All versions match: $CARGO_VERSION"
