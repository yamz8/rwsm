#!/bin/bash
# Compares data/ with a uwsm checkout and lists what a release changed.
# usage: tools/sync-upstream.sh [--update] /path/to/uwsm-checkout
#   --update  copy changed and new upstream files into data/
# See docs/upstream-sync.md for the whole procedure.
set -eu
update=0
[ "${1:-}" = --update ] && { update=1; shift; }
src=${1:?usage: $0 [--update] /path/to/uwsm-checkout}
cd "$(dirname "$0")/.."

# upstream dir -> data dir; single files are mapped below
pairs="uwsm-libexec:libexec uwsm-plugins:plugins scripts:scripts man:man
systemd/user:systemd/user systemd/user-preset:systemd/user-preset example-units:example-units"
files="README.md:README.uwsm.md LICENSE:LICENSE.uwsm"

changed=0
report() { echo "$1: $2"; changed=1; }
copy() { [ "$update" = 1 ] && cp -P "$1" "$2" || true; }

for pair in $pairs; do
  up=${pair%%:*} ours=data/${pair#*:}
  for f in "$src/$up"/*; do
    [ -e "$f" ] || [ -L "$f" ] || continue
    b=$(basename "$f")
    case $b in meson.build | *.py) continue ;; esac
    if [ ! -e "$ours/$b" ] && [ ! -L "$ours/$b" ]; then
      report new "$up/$b"; copy "$f" "$ours/$b"
    elif ! cmp -s "$f" "$ours/$b"; then
      report changed "$up/$b"; copy "$f" "$ours/$b"
    fi
  done
  for f in "$ours"/*; do
    [ -e "$src/$up/$(basename "$f")" ] || report "removed upstream" "$up/$(basename "$f")"
  done
done
for pair in $files; do
  up=${pair%%:*} ours=data/${pair#*:}
  cmp -s "$src/$up" "$ours" || { report changed "$up"; copy "$src/$up" "$ours"; }
done

base=$(sed -n 's/.*(commit `\([0-9a-f]*\)`).*/\1/p' data/README.md)
head=$(git -C "$src" rev-parse --short HEAD)
echo
echo "Vendored from $base, checkout is at $head ($(git -C "$src" describe --tags 2>/dev/null || echo untagged))."
if git -C "$src" cat-file -e "$base" 2>/dev/null; then
  echo "Python changes to port (git -C $src diff $base HEAD -- uwsm/):"
  git -C "$src" diff --stat "$base" HEAD -- uwsm/ | sed 's/^/  /'
else
  echo "Commit $base is not in the checkout; clone without --depth to see the Python diff."
fi
[ "$changed" = 0 ] && echo "data/ matches the checkout."
exit 0
