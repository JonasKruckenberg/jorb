#!/usr/bin/env bash
# Fails if the kernel's final .config does not honour every line of the
# fragments it was built from. A kconfig symbol whose dependencies are unmet is
# dropped silently, which would otherwise only show up as a guest that does not
# boot.
#
# Usage: kernel-config-check.sh <config> <fragment>...

set -euo pipefail

config=$1
shift

missing=0
for fragment in "$@"; do
	while IFS= read -r line; do
		case $line in
		'' | '#'*) continue ;;
		esac

		symbol=${line%%=*}
		value=${line#*=}

		if [ "$value" = n ]; then
			if grep -q "^${symbol}=" "$config"; then
				echo "set but should not be: $(grep "^${symbol}=" "$config")"
				missing=$((missing + 1))
			fi
		elif ! grep -qxF "$line" "$config"; then
			echo "missing: $line (config says: $(grep "^${symbol}[= ]" "$config" || echo 'nothing'))"
			missing=$((missing + 1))
		fi
	done <"$fragment"
done

if [ "$missing" -ne 0 ]; then
	echo "FAIL kernel config: $missing options are not honoured"
	exit 1
fi

echo "PASS kernel config: every option in $* is honoured"
