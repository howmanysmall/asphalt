#!/bin/sh
# Runs the given command, suppressing its output when it succeeds and
# forwarding it to stderr on failure. Used by hk's pre-push hook so that
# pushes stay quiet unless the check actually fails.
set -eu

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT

if "$@" > "$tmp" 2>&1; then
	exit 0
fi

status=$?
cat "$tmp" >&2
exit "$status"
