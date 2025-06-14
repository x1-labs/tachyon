#!/usr/bin/env bash
set -x
set -e
here=$(dirname "$0")

# shellcheck source=.buildkite/scripts/common.sh
source "$here"/common.sh

agent="${1-solana}"

partitions=$(
  cat <<EOF
{
  "name": "partitions",
  "command": "ci/stable/run-partition.sh",
  "timeout_in_minutes": 30,
  "agent": "$agent",
  "parallelism": 2,
  "retry": 3
}
EOF
)

local_cluster_partitions=$(
  cat <<EOF
{
  "name": "local-cluster",
  "command": "ci/stable/run-local-cluster-partially.sh",
  "timeout_in_minutes": 30,
  "agent": "$agent",
  "parallelism": 10,
  "retry": 3
}
EOF
)

localnet=$(
  cat <<EOF
{
  "name": "localnet",
  "command": "ci/stable/run-localnet.sh",
  "timeout_in_minutes": 30,
  "agent": "$agent"
}
EOF
)

# shellcheck disable=SC2016
group "stable" "$partitions" "$local_cluster_partitions" "$localnet"
