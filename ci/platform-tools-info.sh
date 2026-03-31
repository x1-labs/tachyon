#!/usr/bin/env bash
#
# Finds the version of platform-tools used by this source tree.
#
# stdout of this script may be eval-ed.
#

here="$(dirname "$0")"

PLATFORM_TOOLS_VERSION=unknown

# X1: parse the version from the in-repo source instead of executing the
# ../cargo-build-sbf wrapper. The wrapper cargo-installs cargo-build-sbf from
# crates.io on first use, which requires network access and OpenSSL headers on
# the bare CI agent (hooks run outside the docker image) and, when it fails,
# poisons this script's eval-ed stdout. Error chatter goes to stderr for the
# same reason.
toolchain_rs="${here}/../platform-tools-sdk/cargo-build-sbf/src/toolchain.rs"
if [[ -f "${toolchain_rs}" ]]; then
    version=$(sed -e 's/^.*DEFAULT_PLATFORM_TOOLS_VERSION[^"]*"\(v[0-9.][0-9.]*\)".*/\1/;t;d' "${toolchain_rs}")
    if [[ ${version} != '' ]]; then
        PLATFORM_TOOLS_VERSION="${version}"
    else
        echo '--- unable to parse PLATFORM_TOOLS_VERSION' >&2
    fi
else
    echo "--- '${toolchain_rs}' not present" >&2
fi

echo PLATFORM_TOOLS_VERSION="${PLATFORM_TOOLS_VERSION}"
