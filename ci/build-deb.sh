#!/usr/bin/env bash

set -e

# Source cargo environment if not in PATH
# shellcheck source=/dev/null
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"

S3_BUCKET="x1-tachyon-release"
S3_PATH="debs"
AWS_PROFILE="${AWS_PROFILE:-}"

# Target architecture (e.g., x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu)
TARGET="${TARGET:-}"

command -v cargo-deb >/dev/null || cargo install cargo-deb
command -v dpkg-scanpackages >/dev/null || apt-get install -y dpkg-dev

# Get version from workspace Cargo.toml
VERSION=$(grep -m1 '^version = ' Cargo.toml | sed 's/version = "\(.*\)"/\1/')
# Strip patch version for package naming (e.g., 2.2.19 -> 2.2)
MAJOR_MINOR="${VERSION%.*}"

# Build target args (using array to avoid quoting issues)
TARGET_ARGS=()
if [ -n "$TARGET" ]; then
    TARGET_ARGS=("--target" "$TARGET")
    # Ensure target is installed
    rustup target add "$TARGET" 2>/dev/null || true
    echo "==> Cross-compiling for ${TARGET}"
fi

# Function to get next revision number for a package
get_next_revision() {
    local pkg_name=$1
    local arch=$2
    local existing

    # List existing packages in S3 and find highest revision
    existing=$(aws ${AWS_PROFILE:+--profile "$AWS_PROFILE"} s3 ls "s3://${S3_BUCKET}/${S3_PATH}/pool/main/" 2>/dev/null | \
        grep -oE "${pkg_name}_${VERSION}-[0-9]+_${arch}" | \
        sed "s/${pkg_name}_${VERSION}-\([0-9]*\)_${arch}/\1/" | \
        sort -n | tail -1)

    if [ -z "$existing" ]; then
        # Check if base version exists (without revision)
        if aws ${AWS_PROFILE:+--profile "$AWS_PROFILE"} s3 ls "s3://${S3_BUCKET}/${S3_PATH}/pool/main/${pkg_name}_${VERSION}_${arch}" >/dev/null 2>&1; then
            echo "1"
        else
            echo ""
        fi
    else
        echo $((existing + 1))
    fi
}

# Determine debian architecture from target
get_deb_arch() {
    case "$TARGET" in
        aarch64-unknown-linux-gnu) echo "arm64" ;;
        x86_64-unknown-linux-gnu)  echo "amd64" ;;
        "") echo "amd64" ;;  # native build assumed amd64
        *) echo "amd64" ;;
    esac
}

DEB_ARCH=$(get_deb_arch)

# Build tachyon-validator
REVISION=$(get_next_revision "x1-tachyon-validator${MAJOR_MINOR}" "$DEB_ARCH")
if [ -n "$REVISION" ]; then
    echo "==> Building x1-tachyon-validator${MAJOR_MINOR} ${VERSION}-${REVISION} (${DEB_ARCH})"
    cargo deb -p agave-validator "${TARGET_ARGS[@]}" --deb-revision "$REVISION"
else
    echo "==> Building x1-tachyon-validator${MAJOR_MINOR} ${VERSION} (${DEB_ARCH})"
    cargo deb -p agave-validator "${TARGET_ARGS[@]}"
fi

# Build x1-tools (solana-cli)
REVISION=$(get_next_revision "x1-tools" "$DEB_ARCH")
if [ -n "$REVISION" ]; then
    echo "==> Building x1-tools ${VERSION}-${REVISION} (${DEB_ARCH})"
    cargo deb -p solana-cli "${TARGET_ARGS[@]}" --deb-revision "$REVISION"
else
    echo "==> Building x1-tools ${VERSION} (${DEB_ARCH})"
    cargo deb -p solana-cli "${TARGET_ARGS[@]}"
fi
