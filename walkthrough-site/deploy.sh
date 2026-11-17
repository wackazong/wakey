#!/usr/bin/env bash
# Builds the site, checks it, and copies it to the web server over SSH.
#
#   ./deploy.sh              check, build, upload
#   ./deploy.sh --dry-run    the same, but only list what would be uploaded
#   ./deploy.sh --no-check   skip scripts/check (build only)
#
# Target: the SSH host `wackazong` (see ~/.ssh/config) and its folder
# /wackazong, the document root of wackazong.com. Override with
# DEPLOY_HOST and DEPLOY_DIR.
#
# Files already on the server stay: nothing is deleted there, so files
# that are not part of the site (e.g. icsfix.php) are left alone. Files of
# the site are uploaded when new or changed. A page removed from the site
# therefore stays online until it is deleted by hand.
set -euo pipefail

HOST="${DEPLOY_HOST:-wackazong}"
DIR="${DEPLOY_DIR:-/wackazong}"

dry_run=""
check=1
for arg in "$@"; do
  case "$arg" in
    -n|--dry-run) dry_run="--dry-run" ;;
    --no-check) check=0 ;;
    -h|--help) sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

cd "$(dirname "$0")"

if [[ $check == 1 ]]; then
  # Builds into public/ as well.
  scripts/check
else
  zola build
fi

echo
echo "Uploading public/ to $HOST:$DIR/ ${dry_run:+(dry run)}"
# -r recursive, -l symlinks as links, -t times (so the next run skips
# unchanged files), -z compressed, -v list files. No -p/-o/-g: the server
# gives new files its own owner and permissions. No --delete: see above.
rsync -rltzv $dry_run -e ssh public/ "$HOST:$DIR/"
echo "Done: https://wackazong.com/"
