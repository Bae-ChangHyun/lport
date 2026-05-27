#!/bin/sh
# Helper invoked from demo.tape. Writes a fake "latest version" line into
# lport's update-check cache so the demo can deterministically show / hide
# the update-available notice across renders.
#
# Usage: _set-cache.sh <version-string>
mkdir -p "$HOME/.cache/lport"
printf '%s\n%s\n' "$(date +%s)" "$1" > "$HOME/.cache/lport/update-check"
