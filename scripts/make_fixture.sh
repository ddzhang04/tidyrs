#!/usr/bin/env bash
# Generates a test folder with known duplicates and unique files.
# Usage: scripts/make_fixture.sh [target_dir]
# Default target_dir: ./fixture

set -euo pipefail

TARGET="${1:-./fixture}"
rm -rf "$TARGET"
mkdir -p "$TARGET/sub/deeper" "$TARGET/old"

# Three duplicate groups of varying sizes
head -c 5000  /dev/urandom > "$TARGET/photo.jpg"
cp "$TARGET/photo.jpg" "$TARGET/photo_copy.jpg"
cp "$TARGET/photo.jpg" "$TARGET/sub/photo.jpg"

head -c 50000 /dev/urandom > "$TARGET/installer.dmg"
cp "$TARGET/installer.dmg" "$TARGET/old/installer.dmg"

head -c 200000 /dev/urandom > "$TARGET/big.bin"
cp "$TARGET/big.bin" "$TARGET/sub/deeper/big.bin"

# Unique files (should not appear in dup groups)
head -c 5000  /dev/urandom > "$TARGET/unique_a.bin"
head -c 5000  /dev/urandom > "$TARGET/unique_b.bin"   # same size, different content
head -c 800   /dev/urandom > "$TARGET/tiny.txt"        # below 1KB min_size

echo "fixture created at $TARGET"
echo "expected: 3 duplicate groups (photo×3, installer×2, big×2)"
echo "          5000 + 50000 + 200000 = 255000 bytes reclaimable"
