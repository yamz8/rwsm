#!/bin/sh
# Checks that every copy of the matched uwsm version agrees with
# upstream_version!() in src/config.rs (see docs/upstream-sync.md).
cd "$(dirname "$0")/.." || exit 1
v=$(sed -n '/macro_rules! upstream_version/,/}/s/^ *"\([0-9.]*\)"$/\1/p' src/config.rs)
[ -n "$v" ] || { echo "could not read upstream_version!() from src/config.rs" >&2; exit 1; }
fail=0
check() {
    if grep -qF -- "$2" "$1"; then
        echo "ok: $1"
    else
        echo "$1: expected \"$2\"" >&2
        fail=1
    fi
}
check data/README.md "v$v "
check packaging/arch/PKGBUILD "_upstream=$v"
check .github/workflows/ci.yml "UWSM_REF: v$v"
check README.md "uwsm $v"
exit $fail
