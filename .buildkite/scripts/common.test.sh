#!/usr/bin/env bash

set -e
here=$(dirname "$0")

# shellcheck source=.buildkite/scripts/func-assert-eq.sh
source "$here"/func-assert-eq.sh

# shellcheck source=.buildkite/scripts/common.sh
source "$here"/common.sh

(
  want=$(
    cat <<EOF | indent
- name: "test"
  command: "start.sh"
  plugins:
    - docker#v5.12.0:
        image: "anzaxyz/ci:rust_1.78.0_nightly-2024-05-02"
        workdir: /solana
        propagate-environment: true
        propagate-uid-gid: true
        environment:
          - "RUSTC_WRAPPER=/usr/local/cargo/bin/sccache"
          - BUILDKITE_AGENT_ACCESS_TOKEN
          - AWS_SECRET_ACCESS_KEY
          - AWS_ACCESS_KEY_ID
          - SCCACHE_BUCKET
          - SCCACHE_REGION
          - SCCACHE_S3_KEY_PREFIX
          - BUILDKITE_PARALLEL_JOB
          - BUILDKITE_PARALLEL_JOB_COUNT
          - CI
          - CI_BRANCH
          - CI_BASE_BRANCH
          - CI_TAG
          - CI_BUILD_ID
          - CI_COMMIT
          - CI_JOB_ID
          - CI_PULL_REQUEST
          - CI_REPO_SLUG
          - CRATES_IO_TOKEN
          - THREADS_OVERRIDE
  timeout_in_minutes: 10
  agents:
    queue: "agent"
EOF
  )

  got=$(step '{ "name": "test", "command": "start.sh", "timeout_in_minutes": 10, "agent": "agent"}')

  assert_eq "basic setup" "$want" "$got"
)

(
  want=$(
    cat <<EOF | indent
- name: "test"
  command: "start.sh"
  plugins:
    - docker#v5.12.0:
        image: "anzaxyz/ci:rust_1.78.0_nightly-2024-05-02"
        workdir: /solana
        propagate-environment: true
        propagate-uid-gid: true
        environment:
          - "RUSTC_WRAPPER=/usr/local/cargo/bin/sccache"
          - BUILDKITE_AGENT_ACCESS_TOKEN
          - AWS_SECRET_ACCESS_KEY
          - AWS_ACCESS_KEY_ID
          - SCCACHE_BUCKET
          - SCCACHE_REGION
          - SCCACHE_S3_KEY_PREFIX
          - BUILDKITE_PARALLEL_JOB
          - BUILDKITE_PARALLEL_JOB_COUNT
          - CI
          - CI_BRANCH
          - CI_BASE_BRANCH
          - CI_TAG
          - CI_BUILD_ID
          - CI_COMMIT
          - CI_JOB_ID
          - CI_PULL_REQUEST
          - CI_REPO_SLUG
          - CRATES_IO_TOKEN
          - THREADS_OVERRIDE
  timeout_in_minutes: 10
  agents:
    queue: "agent"
  parallelism: 3
EOF
  )

  got=$(step '{ "name": "test", "command": "start.sh", "timeout_in_minutes": 10, "agent": "agent", "parallelism": 3}')

  assert_eq "basic setup + parallelism" "$want" "$got"
)

(
  want=$(
    cat <<EOF | indent
- name: "test"
  command: "start.sh"
  plugins:
    - docker#v5.12.0:
        image: "anzaxyz/ci:rust_1.78.0_nightly-2024-05-02"
        workdir: /solana
        propagate-environment: true
        propagate-uid-gid: true
        environment:
          - "RUSTC_WRAPPER=/usr/local/cargo/bin/sccache"
          - BUILDKITE_AGENT_ACCESS_TOKEN
          - AWS_SECRET_ACCESS_KEY
          - AWS_ACCESS_KEY_ID
          - SCCACHE_BUCKET
          - SCCACHE_REGION
          - SCCACHE_S3_KEY_PREFIX
          - BUILDKITE_PARALLEL_JOB
          - BUILDKITE_PARALLEL_JOB_COUNT
          - CI
          - CI_BRANCH
          - CI_BASE_BRANCH
          - CI_TAG
          - CI_BUILD_ID
          - CI_COMMIT
          - CI_JOB_ID
          - CI_PULL_REQUEST
          - CI_REPO_SLUG
          - CRATES_IO_TOKEN
          - THREADS_OVERRIDE
  timeout_in_minutes: 10
  agents:
    queue: "agent"
  retry:
    automatic:
      - limit: 3
EOF
  )

  got=$(step '{ "name": "test", "command": "start.sh", "timeout_in_minutes": 10, "agent": "agent", "retry": 3}')

  assert_eq "basic setup + retry" "$want" "$got"
)

(
  want=$(
    cat <<EOF | indent
- name: "test"
  command: "start.sh"
  plugins:
    - docker#v5.12.0:
        image: "anzaxyz/ci:rust_1.78.0_nightly-2024-05-02"
        workdir: /solana
        propagate-environment: true
        propagate-uid-gid: true
        environment:
          - "RUSTC_WRAPPER=/usr/local/cargo/bin/sccache"
          - BUILDKITE_AGENT_ACCESS_TOKEN
          - AWS_SECRET_ACCESS_KEY
          - AWS_ACCESS_KEY_ID
          - SCCACHE_BUCKET
          - SCCACHE_REGION
          - SCCACHE_S3_KEY_PREFIX
          - BUILDKITE_PARALLEL_JOB
          - BUILDKITE_PARALLEL_JOB_COUNT
          - CI
          - CI_BRANCH
          - CI_BASE_BRANCH
          - CI_TAG
          - CI_BUILD_ID
          - CI_COMMIT
          - CI_JOB_ID
          - CI_PULL_REQUEST
          - CI_REPO_SLUG
          - CRATES_IO_TOKEN
          - THREADS_OVERRIDE
  timeout_in_minutes: 10
  agents:
    queue: "agent"
  parallelism: 3
  retry:
    automatic:
      - limit: 3
EOF
  )

  got=$(step '{ "name": "test", "command": "start.sh", "timeout_in_minutes": 10, "agent": "agent", "parallelism": 3, "retry": 3}')

  assert_eq "basic setup + parallelism + retry" "$want" "$got"
)
