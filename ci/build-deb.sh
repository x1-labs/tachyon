#!/usr/bin/env bash

set -e

command -v cargo-deb >/dev/null || cargo install cargo-deb
command -v dpkg-scanpackages >/dev/null || apt install -y dpkg-dev

cargo deb -p tachyon-validator
cargo deb -p solana-cli
