#!/usr/bin/env bash
# T5's host check: jorb-init runs through all seven of its steps in a real
# guest and logs JSON to the journal.
#
# Uses G4's harness and the same test image, booting init=/jorb-init instead
# of the busybox init. The image's /usr/local/bin/amp is a stand-in that
# reports what it was handed, so the journal shows what jorb-init did.

here=$(dirname "$0")
# shellcheck source-path=SCRIPTDIR
. "$here/lib.sh"

need_root
need_kvm

slot=${JORB_SLOT:-0}
id=${JORB_GATE_ID:-t5}
token=t5-dummy-token

trap stop_test_vm EXIT
start_test_vm "$id" "$slot" boot /jorb-init "ORB_TOKEN=$token"

if ! wait_for_marker 'amp token' "${JORB_GATE_DEADLINE:-90}"; then
	evidence "last journal lines:"
	tail -n 30 "$gate_journal" | sed 's/^/      /'
	fail "bootstrap never started, so jorb-init did not get through its steps"
fi

evidence "bootstrap reached after ${gate_marker_wait}s"

# Step 7 logs as JSON. The console it shares with the kernel does not, so the
# event is extracted from the line rather than assumed to be the whole of it.
started=$(json_event 'bootstrap started')
if [ -z "$started" ]; then
	evidence "the journal line was: $(grep -m1 'bootstrap started' "$gate_journal" || true)"
	fail "jorb-init logged no 'bootstrap started' JSON event"
fi
if ! printf '%s' "$started" | jq -e '.fields.message == "bootstrap started"' >/dev/null 2>&1; then
	evidence "the event was: $started"
	fail "jorb-init's log line is not the JSON the dashboards expect"
fi
evidence "jorb-init logged: $started"

# Step 6: setpriv dropped to the image's user, in its home.
user=$(marker 'amp user')
home=$(marker 'amp home')
cwd=$(marker 'amp cwd')
evidence "amp ran as $user, HOME=$home, cwd=$cwd"
if [ "$user" != "1000:1000" ]; then
	fail "bootstrap ran as $user, not as the image's user 1000:1000"
fi
if [ "$home" != /home/user ] || [ "$cwd" != /home/user ]; then
	fail "bootstrap ran with HOME=$home in $cwd, not in /home/user"
fi

# Step 4: the config disk's keys reached the child.
if [ "$(marker 'amp token')" != present ]; then
	fail "ORB_TOKEN did not reach bootstrap"
fi
evidence "argv was: $(marker 'amp argv')"

# Step 3 and step 2.
evidence "resolv.conf: $(marker 'amp resolv')"
if ! marker 'amp resolv' | grep -q nameserver; then
	fail "/etc/resolv.conf has no nameserver, so /proc/net/pnp was not copied"
fi

dirty=$(marker 'amp dirty')
evidence "dirty_expire/dirty_writeback: $dirty"
if [ "$dirty" != "100/100" ]; then
	fail "the writeback window is $dirty, not 100/100"
fi

# Step 1.
mounts=$(marker 'amp mounts')
evidence "mounts: $mounts"
for mountpoint in /proc /sys /dev/pts /dev/shm /run; do
	if ! printf '%s' "$mounts" | grep -qw -- "$mountpoint"; then
		fail "$mountpoint is not mounted, so step 1 did not finish"
	fi
done

for probe in dev-stderr dev-fd pty hostname; do
	result=$(marker "amp $probe")
	evidence "$probe: $result"
	if [ "$result" != works ]; then
		fail "$probe does not work in the guest"
	fi
done

pass "jorb-init mounted, set the writeback window, wrote resolv.conf, passed the token and ran bootstrap as 1000:1000 in /home/user"
