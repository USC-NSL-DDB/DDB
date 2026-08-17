#!/usr/bin/env bash
set -euo pipefail

release_ddb_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
release_repo_root="$(cd "${release_ddb_root}/.." && pwd)"
release_output="${1:-${release_repo_root}/dist}"
release_epoch="${SOURCE_DATE_EPOCH:-1704067200}"

manifest_version() {
  awk '
    /^\[package\]/ { in_package=1; next }
    /^\[/ && in_package { exit }
    in_package && /^version[[:space:]]*=/ {
      gsub(/.*=[[:space:]]*"/, "")
      gsub(/".*/, "")
      print
      exit
    }
  ' "$1"
}

ddb_version="$(manifest_version "${release_ddb_root}/core/Cargo.toml")"
tui_version="$(manifest_version "${release_repo_root}/ddb-tui/Cargo.toml")"
release_target="$(rustc -vV | sed -n 's/^host: //p')"
bundle_name="ddb-${ddb_version}_ddb-tui-${tui_version}_${release_target}"
release_tmp="$(mktemp -d)"
bundle_root="${release_tmp}/${bundle_name}"
trap 'rm -rf "${release_tmp}"' EXIT

cargo build --manifest-path "${release_ddb_root}/Cargo.toml" -p ddb --release
cargo build --manifest-path "${release_repo_root}/ddb-tui/Cargo.toml" --release

mkdir -p \
  "${bundle_root}/bin" \
  "${bundle_root}/completions" \
  "${bundle_root}/docs" \
  "${bundle_root}/examples/managed" \
  "${release_output}"

install -m 0755 "${release_ddb_root}/target/release/ddb" "${bundle_root}/bin/ddb"
install -m 0755 "${release_repo_root}/ddb-tui/target/release/ddb-tui" \
  "${bundle_root}/bin/ddb-tui"
install -m 0644 "${release_repo_root}/README.md" "${bundle_root}/README.md"
install -m 0644 "${release_repo_root}/ddb-tui/README.md" \
  "${bundle_root}/docs/ddb-tui.md"
install -m 0644 "${release_ddb_root}/docs/ddb-tui-user-guide.md" \
  "${bundle_root}/docs/user-guide.md"
install -m 0644 "${release_ddb_root}/docs/releases/ddb-tui-integrated-0.1.md" \
  "${bundle_root}/docs/release-notes.md"
install -m 0644 \
  "${release_ddb_root}/docs/ddb-tui-integrated-usability-readiness-2026-08-15.md" \
  "${bundle_root}/docs/readiness.md"
install -m 0644 \
  "${release_ddb_root}/docs/api/adr/0006-two-binaries-one-command.md" \
  "${bundle_root}/docs/architecture.md"
install -m 0644 "${release_ddb_root}/completions/ddb.bash" \
  "${bundle_root}/completions/ddb.bash"
install -m 0644 "${release_ddb_root}/examples/managed/mock.yaml" \
  "${bundle_root}/examples/managed/mock.yaml"
install -m 0644 "${release_ddb_root}/examples/managed/mock_source.rs" \
  "${bundle_root}/examples/managed/mock_source.rs"

license_json='[]'
release_eligible=false
if [[ -f "${release_repo_root}/LICENSE" ]]; then
  install -m 0644 "${release_repo_root}/LICENSE" "${bundle_root}/LICENSE"
  license_json='["LICENSE"]'
  release_eligible=true
else
  echo "warning: no project LICENSE exists; archive is a testable release candidate, not an official open-source release" >&2
fi

cat >"${bundle_root}/manifest.json" <<MANIFEST
{
  "bundle_format": 1,
  "ddb_version": "${ddb_version}",
  "ddb_tui_version": "${tui_version}",
  "target": "${release_target}",
  "supported_api_versions": ["v2"],
  "supported_schema_range": {"minimum": "2.0.0", "maximum_exclusive": "3.0.0"},
  "binaries": ["bin/ddb", "bin/ddb-tui"],
  "license_files": ${license_json},
  "official_release_eligible": ${release_eligible}
}
MANIFEST

create_archive() {
  local archive_output="$1"
  tar --sort=name --mtime="@${release_epoch}" --owner=0 --group=0 --numeric-owner \
    -C "${release_tmp}" -cf - "${bundle_name}" |
    gzip -n >"${archive_output}"
}

archive="${release_output}/${bundle_name}.tar.gz"
reproduction="${release_tmp}/${bundle_name}.reproduced.tar.gz"
create_archive "${archive}"
create_archive "${reproduction}"
if ! cmp -s "${archive}" "${reproduction}"; then
  echo "paired release archive is not byte-for-byte reproducible" >&2
  exit 1
fi

sha256sum "${archive}" >"${archive}.sha256"
printf '%s\n' "${archive}"
