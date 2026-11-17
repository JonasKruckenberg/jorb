#!/usr/bin/env bash
# G8: how an `amp` CLI that has `orb provider` gets onto this host.
#
# Reports what is available rather than installing anything: whether nixpkgs
# has a package, whether an amp on PATH already has the subcommand, and
# whether the official installer is reachable. Resolves A12.

here=$(dirname "$0")
# shellcheck source-path=SCRIPTDIR
. "$here/lib.sh"

found=()

if command -v amp >/dev/null 2>&1; then
	evidence "amp on PATH: $(command -v amp) ($(amp --version 2>&1 | head -n 1))"
	if amp orb --help >/dev/null 2>&1; then
		evidence "that amp has the orb subcommand"
		found+=("amp on PATH")
	else
		evidence "that amp has no orb subcommand"
	fi
else
	evidence "no amp on PATH"
fi

# A package only counts if its amp has the subcommand: nixpkgs#amp is a text
# editor that happens to share the name. amp-cli is unfree, so building it
# needs the environment to say so.
for attribute in amp-cli amp; do
	if ! version=$(nix eval --raw "nixpkgs#$attribute.version" 2>/dev/null); then
		evidence "nixpkgs has no $attribute"
		continue
	fi

	description=$(nix eval --raw "nixpkgs#$attribute.meta.description" 2>/dev/null || echo "no description")
	evidence "nixpkgs has $attribute $version ($description)"

	if ! path=$(NIXPKGS_ALLOW_UNFREE=1 nix build --impure --no-link --print-out-paths "nixpkgs#$attribute" 2>/dev/null); then
		evidence "  it does not build here (unfree, or broken on $(uname -m))"
		continue
	fi

	if [ ! -x "$path/bin/amp" ]; then
		evidence "  it installs no bin/amp"
		continue
	fi

	if timeout 60 "$path/bin/amp" orb provider --help >/dev/null 2>&1; then
		evidence "  its amp has orb provider"
		found+=("nixpkgs#$attribute")
	else
		evidence "  its amp has no orb provider subcommand"
	fi
done

installer=https://ampcode.com/install.sh
if curl --silent --show-error --head --fail "$installer" >/dev/null 2>&1; then
	evidence "the official installer at $installer is reachable"
	evidence "it installs a self-updating dynamic binary, so on NixOS it needs an FHS wrapper"
	found+=("$installer in an FHS wrapper")
else
	evidence "the official installer at $installer is not reachable from here"
fi

if [ "${#found[@]}" -eq 0 ]; then
	fail "no way to install an amp CLI with orb provider was found, so A12 does not hold"
fi

pass "amp with orb provider can come from: ${found[*]}"
