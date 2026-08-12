#!/usr/bin/env bash
# Display the aggregate tables produced by an artifact run.
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

result="${1:-$RESULTS_ROOT/latest}"
[[ -e "$result" ]] || die "result directory not found: $result
  Run ./artifact.sh smoke or ./artifact.sh run first."
result="$(cd -P "$result" && pwd -P)"

echo "=== DDB latency artifact results ==="
echo "Directory: $result"

found=0
while IFS= read -r summary; do
  found=1
  show_csv "$summary"
done < <(find "$result" -maxdepth 2 -type f -name call-depth-summary.csv | sort)

[[ "$found" -eq 1 ]] || die "no aggregate CSV tables found under $result"
