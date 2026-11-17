#!/usr/bin/env bash
# G7: a unit in jorb.slice is visible in the owner's observability stack.
#
# Starts a transient unit that logs a marker and waits for its cgroup metrics
# to appear in Prometheus and its line to appear in Loki. Resolves A11.

here=$(dirname "$0")
# shellcheck source-path=SCRIPTDIR
. "$here/lib.sh"

need_root

prometheus=${JORB_PROMETHEUS_URL:-http://localhost:9090}
loki=${JORB_LOKI_URL:-http://localhost:3100}
unit=jorb-gate-g7
marker="JORB-GATE-G7-$(date -u +%s)"
window=${JORB_GATE_WINDOW:-120}

cleanup() {
	systemctl stop "$unit.service" >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup

systemd-run --collect --unit "$unit" --slice jorb.slice \
	-p Type=exec -- /bin/sh -c "echo $marker; sleep $((window + 30))" >/dev/null

cgroup=$(systemctl show -p ControlGroup --value "$unit.service")
evidence "unit     $unit.service"
evidence "cgroup   $cgroup"
evidence "marker   $marker"

query() {
	curl --silent --show-error --get --data-urlencode "query=$1" "$2"
}

waited=0
metrics=0
logs=0
while [ "$waited" -lt "$window" ]; do
	if [ "$metrics" -eq 0 ]; then
		if query "container_memory_usage_bytes{id=~\"/jorb.slice.*\"}" \
			"$prometheus/api/v1/query" | jq -e '.data.result | length > 0' >/dev/null 2>&1; then
			metrics=1
			evidence "Prometheus has cgroup metrics for /jorb.slice after ${waited}s"
		fi
	fi

	if [ "$logs" -eq 0 ]; then
		if curl --silent --show-error --get \
			--data-urlencode "query={unit=\"$unit.service\"} |= \`$marker\`" \
			--data-urlencode 'limit=1' \
			"$loki/loki/api/v1/query_range" | jq -e '.data.result | length > 0' >/dev/null 2>&1; then
			logs=1
			evidence "Loki has the marker after ${waited}s"
		fi
	fi

	if [ "$metrics" -eq 1 ] && [ "$logs" -eq 1 ]; then
		break
	fi

	sleep 5
	waited=$((waited + 5))
done

if [ "$metrics" -eq 0 ]; then
	evidence "Prometheus answered: $(query 'up' "$prometheus/api/v1/query" | head -c 200)"
	fail "no /jorb.slice cgroup metrics in Prometheus within ${window}s; is nix/jorb.alloy installed?"
fi

if [ "$logs" -eq 0 ]; then
	fail "the journal marker did not reach Loki within ${window}s"
fi

pass "jorb.slice cgroup metrics are in Prometheus and the unit's journal is in Loki"
