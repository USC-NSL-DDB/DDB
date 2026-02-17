#!/bin/bash

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
lib="${here}/../../libfaketime/src/libfaketime.so.1"
bin="${here}/bin/faketime_immature_wakeup"

tmpdir="$(mktemp -d)"
trap 'rm -rf "${tmpdir}"' EXIT

tsfile="${tmpdir}/faketime_timestamp"
printf "+0\n" > "${tsfile}"

export LD_PRELOAD="${lib}"
export FAKETIME_TIMESTAMP_FILE="${tsfile}"
export FAKETIME_NO_CACHE=1

exec "${bin}"
