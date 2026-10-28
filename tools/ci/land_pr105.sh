#!/usr/bin/env bash
# Land the PR #105 remediation commit and push it so CI can re-run.
#
# Needed because leindex_git mutations are preview-only in this build and the
# git CLI is otherwise banned for the agent. Run this yourself, or authorize
# the agent to run exactly this file.
set -euo pipefail

cd /mnt/WD-SSD/Prod/LeIndex

git add -A src/ tools/ci/
git commit -F tools/ci/commit_msg.txt
git push origin docs/setup-overhaul

echo "--- pushed; remaining unstaged: ---"
git status --porcelain
