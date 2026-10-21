"""Check the exact files promoted from the packaging job into a release."""

import hashlib
import json
import os
from pathlib import Path
import sys


version = os.environ["RELEASE_VERSION"]
directory = Path("dist")
expected = {
    "Install.exe",
    "sbdsl.exe",
    f"scratchdsl-{version}-windows-x64.zip",
    f"scratchdsl-{version}-win32-x64.vsix",
    "SHA256SUMS.txt",
    "release-manifest.json",
    "THIRD_PARTY_NOTICES.txt",
    "LICENSE",
}
actual = {item.name for item in directory.iterdir() if item.is_file()}
if actual != expected:
    raise SystemExit(f"Release asset set differs: expected {sorted(expected)}, got {sorted(actual)}")

files = {}
for name in expected:
    data = (directory / name).read_bytes()
    files[name] = (len(data), hashlib.sha256(data).hexdigest())

manifest = json.loads((directory / "release-manifest.json").read_text(encoding="utf-8-sig"))
if manifest["version"] != version or manifest["gitCommit"] != os.environ["GITHUB_SHA"]:
    raise SystemExit("Release manifest version or source commit differs from this workflow")
manifest_assets = manifest["assets"]
if manifest["platform"] != "windows-x64" or {item["name"] for item in manifest_assets} != (
    expected - {"SHA256SUMS.txt", "release-manifest.json"}
):
    raise SystemExit("Release manifest platform or asset set differs from the package")
for asset in manifest_assets:
    size, digest = files[asset["name"]]
    if asset["bytes"] != size or asset["sha256"] != digest:
        raise SystemExit(f"Release manifest checksum or size differs: {asset['name']}")

if (directory / "LICENSE").read_bytes() != Path("LICENSE").read_bytes():
    raise SystemExit("Release license differs from the source license")

checksum_lines = (directory / "SHA256SUMS.txt").read_text(encoding="utf-8-sig").splitlines()
expected_checksums = {
    f"{files[name][1]}  {name}"
    for name in expected - {"SHA256SUMS.txt"}
}
if set(checksum_lines) != expected_checksums:
    raise SystemExit("Release checksum list differs from the validated package")

if len(sys.argv) > 1:
    uploaded = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))["assets"]
    if {item["name"] for item in uploaded} != expected:
        raise SystemExit("Uploaded release asset set differs from the validated package")
    for asset in uploaded:
        size, expected_digest = files[asset["name"]]
        if asset["size"] != size:
            raise SystemExit(f"Uploaded asset size differs: {asset['name']}")
        digest = asset.get("digest")
        if digest and digest != "sha256:" + expected_digest:
            raise SystemExit(f"Uploaded asset checksum differs: {asset['name']}")

print(f"Verified {len(expected)} release assets for ScratchDSL {version}")
