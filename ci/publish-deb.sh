#!/usr/bin/env bash

set -e

S3_BUCKET="x1-tachyon-release"
S3_PATH="debs"
REPO_DIR="$(mktemp -d)"
DIST="stable"
COMPONENT="main"
ARCH="amd64"
AWS_PROFILE="${AWS_PROFILE:-default}"

cleanup() {
    rm -rf "$REPO_DIR"
}
trap cleanup EXIT

echo "==> Syncing existing repo from S3..."
aws --profile $AWS_PROFILE s3 sync "s3://${S3_BUCKET}/${S3_PATH}/" "$REPO_DIR/" --delete 2>/dev/null || true

echo "==> Setting up repo structure..."
mkdir -p "$REPO_DIR/pool/${COMPONENT}"
mkdir -p "$REPO_DIR/dists/${DIST}/${COMPONENT}/binary-${ARCH}"

echo "==> Copying new .deb files to pool..."
cp target/debian/*.deb "$REPO_DIR/pool/${COMPONENT}/"

echo "==> Generating Packages file..."
cd "$REPO_DIR"
apt-ftparchive packages "pool/${COMPONENT}" > "dists/${DIST}/${COMPONENT}/binary-${ARCH}/Packages"
gzip -k -f "dists/${DIST}/${COMPONENT}/binary-${ARCH}/Packages"

echo "==> Generating Release files..."
cat > apt-ftparchive.conf << CONF
APT::FTPArchive::Release::Origin "X1 Labs";
APT::FTPArchive::Release::Label "X1 Tachyon";
APT::FTPArchive::Release::Suite "${DIST}";
APT::FTPArchive::Release::Codename "${DIST}";
APT::FTPArchive::Release::Architectures "${ARCH}";
APT::FTPArchive::Release::Components "${COMPONENT}";
CONF

apt-ftparchive -c apt-ftparchive.conf release "dists/${DIST}" > "dists/${DIST}/Release"
rm apt-ftparchive.conf

# Sign the Release file if GPG key is available
if [ -n "$GPG_KEY_ID" ]; then
    echo "==> Signing Release file..."
    gpg --batch --yes --default-key "$GPG_KEY_ID" --armor --detach-sign --output "dists/${DIST}/Release.gpg" "dists/${DIST}/Release"
    gpg --batch --yes --default-key "$GPG_KEY_ID" --armor --clearsign --output "dists/${DIST}/InRelease" "dists/${DIST}/Release"
fi

echo "==> Syncing repo back to S3..."
aws --profile $AWS_PROFILE s3 sync --acl public-read "$REPO_DIR/" "s3://${S3_BUCKET}/${S3_PATH}/" --delete

echo "==> Done! Repository published to https://release.x1.xyz/${S3_PATH}/"
echo ""
echo "# To use this repository:"
echo "curl -fsSL https://release.x1.xyz/x1-archive-keyring-binary.gpg > /usr/share/keyrings/x1-archive-keyring.gpg"
echo "echo \"deb [signed-by=/usr/share/keyrings/x1-archive-keyring.gpg] https://release.x1.xyz/${S3_PATH} ${DIST} ${COMPONENT}\" | tee /etc/apt/sources.list.d/x1-tachyon.list"
echo "# To install packages:"
echo "apt update && apt install tachyon-validator x1-tools"
