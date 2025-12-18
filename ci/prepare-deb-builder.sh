#!/usr/bin/env bash

set -e

AWS_PROFILE="${AWS_PROFILE:-default}"
SSM_REGION="${SSM_REGION:-us-west-2}"
SSM_KEY="${SSM_KEY:-/tachyon-ci/gpg/5CCBB18911F15DDD}"

SUDO=""
if [ "$(id -u)" -ne 0 ]; then
    SUDO="sudo"
fi

export DEBIAN_FRONTEND=noninteractive
$SUDO apt-get update
$SUDO apt-get install -y --no-install-recommends apt-utils unzip gnupg ca-certificates dpkg-dev git curl libssl-dev libudev-dev pkg-config zlib1g-dev llvm clang cmake make libprotobuf-dev protobuf-compiler

curl https://sh.rustup.rs -sSf | sh -s -- -y
. "$HOME/.cargo/env"
export PATH="$HOME/.cargo/bin:$PATH"
cargo install cargo-deb

# Install AWS
curl "https://awscli.amazonaws.com/awscli-exe-linux-x86_64.zip" -o "awscliv2.zip"
unzip -f awscliv2.zip
$SUDO ./aws/install --update

# Import GPG key from SSM
echo aws ssm get-parameter \
       --profile "$AWS_PROFILE" \
       --name "$SSM_KEY" \
       --with-decryption \
       --query 'Parameter.Value' \
       --output text

GPG_KEY=$(aws ssm get-parameter \
  --profile "$AWS_PROFILE" \
  --name "$SSM_KEY" \
  --with-decryption \
  --query 'Parameter.Value' \
  --output text)
echo "$GPG_KEY" | gpg --batch --import
