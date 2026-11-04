#!/usr/bin/env bash
set -euo pipefail
task_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ -x "$task_root/.tools/cargo/bin/cargo" ]]; then
  export RUSTUP_HOME="$task_root/.tools/rustup"
  export CARGO_HOME="$task_root/.tools/cargo"
  export PATH="$task_root/.tools/cargo/bin:$PATH"
  exec "$task_root/.tools/cargo/bin/cargo" "$@"
fi
exec cargo "$@"
