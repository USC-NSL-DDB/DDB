# Paired DDB and ddb-tui release-candidate evidence

Date: 2026-08-15
Target: x86_64-unknown-linux-gnu
Result: PASS as a technical release candidate; official release eligibility is
blocked by the absent project license.

## Artifact

~~~text
/tmp/ddb-paired-release-final/ddb-0.1.15_ddb-tui-0.1.0_x86_64-unknown-linux-gnu.tar.gz
SHA-256 b01fb6b3ae789921ec9402c0b0a2886672466d311fa7433d537487a92926b34c
~~~

The companion .sha256 file was compared with a fresh sha256sum invocation and
matched exactly. The packaging script also generated a second archive from the
same staged tree and required byte-for-byte equality before publishing the
artifact.

## Embedded manifest

~~~json
{
  "bundle_format": 1,
  "ddb_version": "0.1.15",
  "ddb_tui_version": "0.1.0",
  "target": "x86_64-unknown-linux-gnu",
  "supported_api_versions": ["v2"],
  "supported_schema_range": {
    "minimum": "2.0.0",
    "maximum_exclusive": "3.0.0"
  },
  "binaries": ["bin/ddb", "bin/ddb-tui"],
  "license_files": [],
  "official_release_eligible": false
}
~~~

## Extracted-artifact smoke

ddb/tools/test-ddb-tui-release.sh passed. The test extracts exactly one bundle
root and uses an empty PATH to verify:

- sibling resolution through ddb tui --help;
- direct ddb-tui resolution through --ddb-path;
- authenticated headless ddb serve readiness and SIGTERM shutdown;
- one-command managed Mock startup through ddb tui; and
- direct managed Mock startup through ddb-tui.

The archive includes both binaries, Bash completion, managed Mock configuration
and source, top-level/TUI documentation, the user guide, release notes,
architecture decision, readiness report, and compatibility/license manifest.
