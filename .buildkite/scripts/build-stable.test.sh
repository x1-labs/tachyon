#!/usr/bin/env bash

set -e
here=$(dirname "$0")

# shellcheck source=.buildkite/scripts/func-assert-eq.sh
source "$here"/func-assert-eq.sh

want=$(
  cat <<'EOF'
  - group: "stable"
    steps:
      - name: "partitions"
        command: "ci/stable/run-partition.sh"
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
        timeout_in_minutes: 30
        agents:
          queue: "solana"
        parallelism: 2
        retry:
          automatic:
            - limit: 3
      - name: "local-cluster"
        command: "ci/stable/run-local-cluster-partially.sh"
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
        timeout_in_minutes: 30
        agents:
          queue: "solana"
        parallelism: 5
        retry:
          automatic:
            - limit: 3
      - name: "localnet"
        command: "ci/stable/run-localnet.sh"
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
        timeout_in_minutes: 30
        agents:
          queue: "solana"
EOF
)

# shellcheck source=.buildkite/scripts/build-stable.sh
got=$(source "$here"/build-stable.sh)

assert_eq "test build stable steps" "$want" "$got"
