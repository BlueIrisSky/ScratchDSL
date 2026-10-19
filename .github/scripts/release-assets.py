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

manifest = json.loads((directory / "release-manifest.json").read_text(encoding="utf-8-sig"))
if manifest["version"] != version or manifest["gitCommit"] != os.environ["GITHUB_SHA"]:
    raise SystemExit("Release manifest version or source commit differs from this workflow")
manifest_assets = manifest["assets"]
if manifest["platform"] != "windows-x64" or {item["name"] for item in manifest_assets} != (
    expected - {"SHA256SUMS.txt", "release-manifest.json"}
) or len(manifest_assets) != len(expected) - 2:
    raise SystemExit("Release manifest platform or asset set differs from the package")
for asset in manifest_assets:
    data = (directory / asset["name"]).read_bytes()
    if asset["bytes"] != len(data) or asset["sha256"] != hashlib.sha256(data).hexdigest():
        raise SystemExit(f"Release manifest checksum or size differs: {asset['name']}")

if (directory / "LICENSE").read_bytes() != Path("LICENSE").read_bytes():
    raise SystemExit("Release license differs from the source license")

checksum_lines = (directory / "SHA256SUMS.txt").read_text(encoding="utf-8-sig").splitlines()
expected_checksums = {
    f"{hashlib.sha256((directory / name).read_bytes()).hexdigest()}  {name}"
    for name in expected - {"SHA256SUMS.txt"}
}
if set(checksum_lines) != expected_checksums or len(checksum_lines) != len(expected_checksums):
    raise SystemExit("Release checksum list differs from the validated package")

if len(sys.argv) > 1:
    uploaded = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))["assets"]
    if {item["name"] for item in uploaded} != expected or len(uploaded) != len(expected):
        raise SystemExit("Uploaded release asset set differs from the validated package")
    for asset in uploaded:
        data = (directory / asset["name"]).read_bytes()
        if asset["size"] != len(data):
            raise SystemExit(f"Uploaded asset size differs: {asset['name']}")
        digest = asset.get("digest")
        if digest and digest != "sha256:" + hashlib.sha256(data).hexdigest():
            raise SystemExit(f"Uploaded asset checksum differs: {asset['name']}")

print(f"Verified {len(expected)} release assets for ScratchDSL {version}")
