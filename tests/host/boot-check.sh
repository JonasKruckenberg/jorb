#!/usr/bin/env bash
# Boots the config jorb itself sends, on this host, with no privileges.
#
# The golden file is what jorb-vmm emits, so a field Cloud Hypervisor requires
# and jorb omits fails here rather than on a real spawn. That is not
# hypothetical: 54.1 refuses a disk with no `image_type`, and nothing in
# `nix flake check` could see it, because the only thing that can say whether a
# config is acceptable is a Cloud Hypervisor.
#
# `vm.create` and `vm.boot` need nothing but /dev/kvm. The tap is the only part
# that needs CAP_NET_ADMIN, so this drops it and leaves the guest with no
# network. That covers A1, A2 and A4 on every run; the tap, the `ip=` address
# and the journal belong to G4, which needs root.

here=$(dirname "$0")
# shellcheck source-path=SCRIPTDIR
. "$here/lib.sh"

need_kvm

work=$(mktemp -d "${TMPDIR:-/tmp}/jorb-boot-check-XXXXXX")
vmm=
trap 'if [ -n "$vmm" ]; then kill "$vmm" 2>/dev/null || true; fi; rm -rf "$work"' EXIT

# marker() reads this, so the VMM's stdout stands in for the journal.
gate_journal=$work/serial.log
golden=$here/../../jorb-vmm/tests/vm-shape.json

kernel=$(built "${JORB_KERNEL_FLAKE:-.#guest-kernel}")/kernel
image=$(built "${JORB_TEST_IMAGE_FLAKE:-.#test-image}")/test.raw
install -m 0600 "$image" "$work/root.raw"
write_config_disk "$work/orb.cfg" "JORB_GATE=boot-check"

evidence "kernel $kernel"
evidence "config $golden"

socket=$work/api.sock
cloud-hypervisor --api-socket "$socket" >"$gate_journal" 2>&1 &
vmm=$!

if ! wait_for_path "$socket" "${JORB_API_SOCKET_WAIT:-15}"; then
	fail "cloud-hypervisor did not open $socket"
fi

# Everything but the paths and the dropped tap is jorb's own config.
config=$(jq \
	--arg kernel "$kernel" \
	--arg root "$work/root.raw" \
	--arg config_disk "$work/orb.cfg" \
	--arg cmdline "$(guest_cmdline 0 boot-check /init)" \
	'.payload.kernel = $kernel
	 | .payload.cmdline = $cmdline
	 | .disks[0].path = $root
	 | .disks[1].path = $config_disk
	 | del(.net)' \
	"$golden")

if ! printf '%s' "$config" | api "$socket" PUT vm.create --data-binary @- >"$work/create"; then
	evidence "vm.create said: $(cat "$work/create")"
	fail "the config jorb sends was refused by vm.create"
fi

if ! api "$socket" PUT vm.boot >"$work/boot"; then
	evidence "vm.boot said: $(cat "$work/boot")"
	fail "the config jorb sends was refused by vm.boot"
fi

waited=0
deadline=${JORB_GATE_DEADLINE:-60}
while [ "$waited" -lt "$deadline" ] && ! grep -q 'JORB-GATE mode' "$gate_journal"; do
	sleep 1
	waited=$((waited + 1))
done

if ! grep -q 'JORB-GATE mode' "$gate_journal"; then
	evidence "last serial lines:"
	tail -n 20 "$gate_journal" | sed 's/^/      /'
	fail "the guest never reached its init, so A1 does not hold as the VM is shaped"
fi

evidence "guest booted after ${waited}s: $(marker booted)"
evidence "disks: $(grep -oE '/dev/vd[abc] (present|absent)' "$gate_journal" | tr '\n' ' ')"

vdb_line=$(marker vdb-first-line)
evidence "first line of /dev/vdb: $vdb_line"
if [ "$vdb_line" != "JORB_GATE=boot-check" ]; then
	fail "/dev/vdb is not the config disk, so A2 does not hold"
fi

guest_epoch=$(marker date)
if [ -z "$guest_epoch" ]; then
	fail "the guest never reported its clock, so A4 cannot be checked"
fi
skew=$((guest_epoch - $(date -u +%s)))
evidence "guest clock $guest_epoch against host $(date -u +%s), skew ${skew}s"
if [ "${skew#-}" -gt 60 ]; then
	fail "the guest clock is ${skew}s off the host's, so A4 does not hold"
fi

pass "the config jorb sends boots: guest up, vdb is the config disk, clock within ${skew}s"
