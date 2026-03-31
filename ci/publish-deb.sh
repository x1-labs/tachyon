#!/usr/bin/env bash

set -e

S3_BUCKET="x1-tachyon-release"
S3_PATH="debs"
REPO_DIR="$(mktemp -d)"
DIST="stable"
COMPONENT="main"
AWS_PROFILE="${AWS_PROFILE:-}"
KEEP_VERSIONS="${KEEP_VERSIONS:-20}"

cleanup() {
    rm -rf "$REPO_DIR"
}
trap cleanup EXIT

echo "==> Syncing existing repo from S3..."
aws ${AWS_PROFILE:+--profile "$AWS_PROFILE"} s3 sync "s3://${S3_BUCKET}/${S3_PATH}/" "$REPO_DIR/" --delete 2>/dev/null || true

echo "==> Setting up repo structure..."
mkdir -p "$REPO_DIR/pool/${COMPONENT}"

echo "==> Copying new .deb files to pool..."
cp target/debian/*.deb "$REPO_DIR/pool/${COMPONENT}/"

echo "==> Pruning old versions (keeping last ${KEEP_VERSIONS})..."
cd "$REPO_DIR/pool/${COMPONENT}"
# Get unique package names (everything before the first _)
for pkg in $(find . -maxdepth 1 -name "*.deb" -printf '%f\n' 2>/dev/null | sed 's/_.*$//' | sort -u); do
    # List all versions of this package, sort by version, keep only old ones to delete
    find . -maxdepth 1 -name "${pkg}_*.deb" -printf '%f\n' 2>/dev/null | sort -V | head -n -"${KEEP_VERSIONS}" | while read -r old_deb; do
        echo "  Removing old: $old_deb"
        rm -f "$old_deb"
    done
done
cd "$REPO_DIR"

# Detect architectures from .deb files in pool
ARCHS=$(find "$REPO_DIR/pool/${COMPONENT}/" -maxdepth 1 -name "*.deb" -printf '%f\n' 2>/dev/null | sed 's/.*_\([^_]*\)\.deb$/\1/' | sort -u | tr '\n' ' ')
ARCHS="${ARCHS:-amd64}"
echo "==> Detected architectures: ${ARCHS}"

echo "==> Generating Packages files..."
cd "$REPO_DIR"
for ARCH in $ARCHS; do
    echo "  Processing ${ARCH}..."
    mkdir -p "dists/${DIST}/${COMPONENT}/binary-${ARCH}"
    apt-ftparchive --arch "$ARCH" packages "pool/${COMPONENT}" > "dists/${DIST}/${COMPONENT}/binary-${ARCH}/Packages"
    gzip -k -f "dists/${DIST}/${COMPONENT}/binary-${ARCH}/Packages"
done

echo "==> Generating Release files..."
ARCH_LIST=$(echo "$ARCHS" | tr ' ' '\n' | paste -sd ' ')
cat > apt-ftparchive.conf << CONF
APT::FTPArchive::Release::Origin "X1 Labs";
APT::FTPArchive::Release::Label "X1 Tachyon";
APT::FTPArchive::Release::Suite "${DIST}";
APT::FTPArchive::Release::Codename "${DIST}";
APT::FTPArchive::Release::Architectures "${ARCH_LIST}";
APT::FTPArchive::Release::Components "${COMPONENT}";
CONF

apt-ftparchive -c apt-ftparchive.conf release "dists/${DIST}" > "dists/${DIST}/Release"
rm apt-ftparchive.conf

# Sign the Release file if GPG key is available
if [ -n "$GPG_KEY_ID" ]; then
    export GNUPGHOME=/tmp/gnupg
    mkdir -p $GNUPGHOME
    chmod 700 $GNUPGHOME
    echo "==> Signing Release file..."
    gpg --batch --yes --default-key "$GPG_KEY_ID" --armor --detach-sign --output "dists/${DIST}/Release.gpg" "dists/${DIST}/Release"
    gpg --batch --yes --default-key "$GPG_KEY_ID" --armor --clearsign --output "dists/${DIST}/InRelease" "dists/${DIST}/Release"
else
    # Remove stale signature files to avoid hash mismatches
    rm -f "dists/${DIST}/Release.gpg" "dists/${DIST}/InRelease"
fi

echo "==> Syncing repo back to S3..."
aws ${AWS_PROFILE:+--profile "$AWS_PROFILE"} s3 sync --acl public-read "$REPO_DIR/" "s3://${S3_BUCKET}/${S3_PATH}/" --delete

echo "==> Done! Repository published to https://release.x1.xyz/${S3_PATH}/"
echo ""
echo "# To use this repository:"
echo "curl -fsSL https://release.x1.xyz/x1-archive-keyring-binary.gpg > /usr/share/keyrings/x1-archive-keyring.gpg"
echo "echo \"deb [signed-by=/usr/share/keyrings/x1-archive-keyring.gpg] https://release.x1.xyz/${S3_PATH} ${DIST} ${COMPONENT}\" | tee /etc/apt/sources.list.d/x1-tachyon.list"
echo "# To install packages:"
echo "apt update && apt install x1-tachyon-validator2.2 x1-tools"
