# shellcheck shell=bash
# How the host scripts report: one PASS or FAIL line, and indented evidence.
# A script never repairs anything; a FAIL is a question for the owner.

set -euo pipefail

script=$(basename "$0" .sh)

evidence() { printf '    %s\n' "$*"; }
pass() {
	printf 'PASS %s: %s\n' "$script" "$*"
	exit 0
}
fail() {
	printf 'FAIL %s: %s\n' "$script" "$*"
	exit 1
}
need_root() { [ "$(id -u)" -eq 0 ] || fail "needs root"; }
built() { nix build --no-link --print-out-paths "$1"; }
