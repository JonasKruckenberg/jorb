# shellcheck shell=bash
# Shared by the gate scripts: how they report, and what they all need.
#
# Each gate prints exactly one PASS or FAIL line, and as many indented
# evidence lines as it takes to believe it. A gate never repairs anything: a
# FAIL is a question for the owner, not a problem to work around.

set -euo pipefail

gate=$(basename "$0" .sh)

evidence() {
	printf '    %s\n' "$*"
}

pass() {
	printf 'PASS %s: %s\n' "$gate" "$*"
	exit 0
}

fail() {
	printf 'FAIL %s: %s\n' "$gate" "$*"
	exit 1
}

need_root() {
	if [ "$(id -u)" -ne 0 ]; then
		fail "needs root (taps, systemd-run and cgroups); run it with sudo"
	fi
}

need_kvm() {
	if [ ! -c /dev/kvm ]; then
		fail "no /dev/kvm on this host"
	fi
}

# The flake output for a derivation the gates need, built on demand.
built() {
	nix build --no-link --print-out-paths "$1"
}

# Bytes to a human size, for evidence lines only.
mib() {
	echo "$(($1 / 1024 / 1024)) MiB"
}

# The console the guest kernel writes to, which differs by architecture.
console_device() {
	case $(uname -m) in
	aarch64) echo ttyAMA0 ;;
	x86_64) echo ttyS0 ;;
	*) fail "no console device known for $(uname -m)" ;;
	esac
}

# The kernel cmdline from the Fixed interfaces section of AGENTS.md.
guest_cmdline() {
	local slot=$1 id=$2 init=$3
	printf 'console=%s root=/dev/vda rw init=%s ip=10.77.%s.2::10.77.%s.1:255.255.255.252:%s:eth0:off:1.1.1.1:9.9.9.9' \
		"$(console_device)" "$init" "$slot" "$slot" "$id"
}

# The VM shape from AGENTS.md, as Cloud Hypervisor's API wants it: root disk
# first and writable, config disk second and read-only, one tap, huge pages and
# THP off, a balloon of nothing that reports freed pages, serial on stdout so
# the journal gets it, virtio console off.
#
# G4 and G5 boot exactly this, independently of jorb's own code.
vm_config() {
	local kernel=$1 cmdline=$2 root_disk=$3 config_disk=$4
	local tap=$5 host_address=$6 netmask=$7 vcpus=$8 memory_mib=$9

	jq -n \
		--arg kernel "$kernel" \
		--arg cmdline "$cmdline" \
		--arg root_disk "$root_disk" \
		--arg config_disk "$config_disk" \
		--arg tap "$tap" \
		--arg host_address "$host_address" \
		--arg netmask "$netmask" \
		--argjson vcpus "$vcpus" \
		--argjson memory "$((memory_mib * 1024 * 1024))" \
		'{
			payload: { kernel: $kernel, cmdline: $cmdline },
			cpus: { boot_vcpus: $vcpus, max_vcpus: $vcpus },
			memory: { size: $memory, hugepages: false, thp: false, shared: false },
			# image_type is required: without it vm.boot answers 400
			# with "Image type required for disk". Every disk here is
			# a raw file.
			disks: [
				{ path: $root_disk, readonly: false, image_type: "Raw" },
				{ path: $config_disk, readonly: true, image_type: "Raw" }
			],
			net: [ { tap: $tap, ip: $host_address, mask: $netmask } ],
			balloon: { size: 0, free_page_reporting: true },
			serial: { mode: "Tty" },
			console: { mode: "Off" }
		}'
}

# A config disk: KEY=VALUE lines, NUL-padded to 4096 bytes, mode 0600.
write_config_disk() {
	local path=$1
	shift
	printf '%s\n' "$@" >"$path"
	truncate -s 4096 "$path"
	chmod 0600 "$path"
}

api() {
	local socket=$1 method=$2 path=$3
	shift 3
	curl --silent --show-error --fail-with-body --unix-socket "$socket" \
		-X "$method" -H 'Content-Type: application/json' "$@" \
		"http://localhost/api/v1/$path"
}

# Waits for a path to appear, up to a deadline in seconds.
wait_for_path() {
	local path=$1 deadline=$2
	local waited=0
	while [ "$waited" -lt "$deadline" ]; do
		if [ -e "$path" ]; then
			return 0
		fi
		sleep 1
		waited=$((waited + 1))
	done
	return 1
}

# shellcheck disable=SC2034 # the gate_* variables are read by the gate scripts
#
# Boots the busybox test image in the final VM shape and leaves it running.
#
# Sets gate_unit, gate_socket, gate_journal, gate_tap, gate_root_disk and
# gate_boot_epoch for the caller, which is what the assertions read. Call
# stop_test_vm when done, whatever the outcome.
start_test_vm() {
	local id=$1 slot=$2 mode=$3 init=$4
	shift 4
	local vcpus=${JORB_VCPUS:-4}
	local memory_mib=${JORB_MEMORY_MIB:-6144}

	gate_unit=jorb-gate-$id
	gate_tap=jorb$slot
	gate_socket=/run/jorb/$id.sock
	gate_config_disk=/run/jorb/$id.cfg
	gate_work=${JORB_GATE_DIR:-/var/tmp/jorb-gate}
	gate_root_disk=$gate_work/$id.raw
	gate_journal=$gate_work/$id.journal
	gate_host_address=10.77.$slot.1
	gate_guest_address=10.77.$slot.2

	# Start from no debris, whatever a previous run left.
	stop_test_vm

	local kernel image
	kernel=$(built "${JORB_KERNEL_FLAKE:-.#guest-kernel}")/kernel
	image=$(built "${JORB_TEST_IMAGE_FLAKE:-.#test-image}")/test.raw

	mkdir -p /run/jorb "$gate_work"
	install -m 0600 "$image" "$gate_root_disk"
	# The marker G4 reads from /dev/vdb comes first; a caller may add its own
	# keys after it.
	write_config_disk "$gate_config_disk" "JORB_GATE=$id" "$@"

	gate_cmdline="$(guest_cmdline "$slot" "$id" "$init") jorb.test=$mode"
	local config
	config=$(vm_config "$kernel" "$gate_cmdline" "$gate_root_disk" "$gate_config_disk" \
		"$gate_tap" "$gate_host_address" 255.255.255.252 "$vcpus" "$memory_mib")

	evidence "kernel   $kernel"
	evidence "cmdline  $gate_cmdline"

	systemd-run --collect --unit "$gate_unit" --slice jorb.slice \
		-p Type=exec -p TimeoutStopSec=5 -p OOMScoreAdjust=500 \
		-- cloud-hypervisor --api-socket "$gate_socket" >/dev/null

	# This run's journal and no other. Filtering by unit alone reads back
	# whatever an earlier run of the same gate left, and the time window this
	# replaces was worse than useless: `date -u` prints UTC while
	# `journalctl --since` reads local time, so on a host an hour ahead of UTC
	# the window opened an hour early and the gate found the previous run's
	# markers on its first poll, in zero seconds.
	gate_invocation=$(systemctl show -p InvocationID --value "$gate_unit.service")

	if ! wait_for_path "$gate_socket" "${JORB_API_SOCKET_WAIT:-15}"; then
		fail "cloud-hypervisor did not open $gate_socket"
	fi

	# Both bodies are kept: a 400 from either carries the reason, and
	# throwing it away leaves nothing but curl's exit code to go on.
	if ! printf '%s' "$config" | api "$gate_socket" PUT vm.create --data-binary @- >"$gate_work/$id.create"; then
		evidence "vm.create said: $(cat "$gate_work/$id.create")"
		fail "vm.create was refused"
	fi
	if ! api "$gate_socket" PUT vm.boot >"$gate_work/$id.boot"; then
		evidence "vm.boot said: $(cat "$gate_work/$id.boot")"
		fail "vm.boot was refused"
	fi
	gate_boot_epoch=$(date -u +%s)
}

stop_test_vm() {
	systemctl stop "$gate_unit.service" >/dev/null 2>&1 || true
	rm -f "$gate_socket" "$gate_config_disk"
	ip link del "$gate_tap" >/dev/null 2>&1 || true
}

# shellcheck disable=SC2034 # gate_marker_wait is read by the gate scripts
#
# Collects the unit's journal into gate_journal and returns once `marker`
# appears, or fails at the deadline.
wait_for_marker() {
	local marker=$1 deadline=$2
	local waited=0
	while [ "$waited" -lt "$deadline" ]; do
		journalctl "_SYSTEMD_INVOCATION_ID=$gate_invocation" \
			--no-pager --output cat >"$gate_journal" 2>/dev/null || true
		if grep -q "JORB-GATE $marker" "$gate_journal"; then
			gate_marker_wait=$waited
			return 0
		fi
		sleep 1
		waited=$((waited + 1))
	done
	gate_marker_wait=$waited
	return 1
}

# The value a marker line carries, or nothing when the marker is absent.
#
# Three things are being undone here, all of them from sharing one serial
# console with the kernel. The leading `.*` drops a kernel message that landed
# in front of the marker; the second sed drops one that landed behind it,
# which is why the match is on the kernel's own `[   13.102431]` timestamp;
# and the CR is there because a serial console terminates every line CRLF, so
# a value compared without stripping it looks identical in evidence and still
# fails. The `|| true` matters as much: under `set -o pipefail` a missing
# marker would otherwise kill the script where the caller's own check is what
# should report it.
marker() {
	grep -m1 "JORB-GATE $1" "$gate_journal" 2>/dev/null |
		sed "s/.*JORB-GATE $1 //" |
		sed 's/\[ *[0-9][0-9]*\.[0-9][0-9]*\].*$//' |
		tr -d '\r' | sed 's/[[:space:]]*$//' || true
}

# The JSON event matching a pattern, with the kernel's console noise removed.
#
# The guest kernel and the guest's userspace share one serial console. printk
# writes straight to the UART while jorb-init writes through the tty, so a
# kernel message can land inside a JSON line, and it is not rare: "Freeing
# unused kernel memory" is flushed as init starts, which is exactly when
# jorb-init logs. The braces are what identifies the event, so whatever the
# kernel printed either side of them is dropped.
json_event() {
	grep -m1 "$1" "$gate_journal" 2>/dev/null | tr -d '\r' | grep -o '{.*}' || true
}

# The memory the unit's cgroup is charged for, in bytes.
unit_memory_current() {
	local cgroup
	cgroup=$(systemctl show -p ControlGroup --value "$gate_unit.service")
	cat "/sys/fs/cgroup$cgroup/memory.current"
}
