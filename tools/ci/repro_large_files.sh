#!/usr/bin/env bash
# Reproduce .github/workflows/quality.yml large-file-check locally, byte-for-byte.
set -u

MAX_LINES=2000
MAX_SIZE_KB=1024
FOUND_ISSUE=0

while IFS= read -r file; do
  lines=$(wc -l < "$file")
  if [ "$lines" -gt "$MAX_LINES" ]; then
    printf 'WARN lines  %5s  %s\n' "$lines" "$file"
    FOUND_ISSUE=1
  fi
done < <(find src -name '*.rs' -not -path '*/target/*' 2>/dev/null)

while IFS= read -r file; do
  size_kb=$(( $(stat --format=%s "$file" 2>/dev/null || stat -f%z "$file" 2>/dev/null || echo 0) / 1024 ))
  if [ "$size_kb" -gt "$MAX_SIZE_KB" ]; then
    printf 'WARN size   %5sKB  %s\n' "$size_kb" "$file"
    FOUND_ISSUE=1
  fi
done < <(find . -type f -not -path './target/*' -not -path './.git/*' -not -path './node_modules/*' -not -path './models/*' -not -name '*.onnx' -not -name '*.bin' 2>/dev/null)

if [ "$FOUND_ISSUE" -eq 1 ]; then
  echo "RESULT: FAIL - large file(s) detected"
  exit 1
fi
echo "RESULT: PASS - no large files detected"
