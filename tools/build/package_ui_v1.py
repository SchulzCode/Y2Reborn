#!/usr/bin/env python3
"""Preserving UI-only root candidate over an exact Platform v1 base. No device I/O."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess

REPO = Path(__file__).resolve().parents[2]
APPLICATION_PATHS = ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "crates", "assets/fonts", "assets/icons", "app/reborn", ":(exclude)app/reborn/src/bin/reborn-preview.rs"]
UI_VERSION = "0.1.0-ui-v1-candidate.1"


def run(*args):
    return subprocess.check_output([str(a) for a in args], stderr=subprocess.PIPE)


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def root_only(scatter):
    chunks = scatter.split("- partition_index:")
    names = []
    for i in range(1, len(chunks)):
        name = re.search(r"partition_name:\s*(\S+)", chunks[i]).group(1)
        names.append(name)
        chunks[i] = re.sub(r"is_download: \S+", "is_download: " + ("true" if name == "ANDROID" else "false"), chunks[i])
        chunks[i] = re.sub(r"file_name: \S+", "file_name: " + ("Y2ROOT.img" if name == "ANDROID" else "NONE"), chunks[i])
    if names.count("ANDROID") != 1:
        raise ValueError("exactly one ANDROID partition required")
    result = "- partition_index:".join(chunks)
    if len(re.findall(r"is_download: true", result)) != 1:
        raise ValueError("ambiguous download selection")
    return result


def elf(path):
    data = path.read_bytes()
    assert data[:6] == b"\x7fELF\x01\x01", (path, "not ELF32 little endian")
    assert int.from_bytes(data[18:20], "little") == 40, (path, "not ARM")
    assert int.from_bytes(data[36:40], "little") & 0x400, (path, "not hard float")
    dynamic = run("readelf", "-d", path).decode()
    return {"sha256": sha(path), "bytes": len(data), "needed": re.findall(r"Shared library: \[(.*?)\]", dynamic)}


def image_read(image, path):
    return run("debugfs", "-R", "cat " + path, image)


def quoted(path):
    text = str(path)
    if any(c in text for c in '\n\r\x00"\\'):
        raise ValueError("unsupported debugfs path")
    return '"' + text + '"'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("platform", "build", "base", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    base = args.base.resolve()
    build = args.build.resolve()
    output = args.output.resolve()
    source = (build / "source-commit.txt").read_text().strip()
    assert re.fullmatch(r"[0-9a-f]{40}", source)
    assert not run("git", "-C", REPO, "diff", source, "--", *APPLICATION_PATHS).strip(), "application source differs from compiled commit"
    # Ignore the owner's unrelated documentation and historical asset deletions;
    # every production input is constrained to the committed source above.
    assert not output.exists(), "fresh preserving output required"
    manifest = json.loads((base / "manifest.json").read_text())
    identities = {}
    for partition, filename in (("ANDROID", "Y2ROOT.img"), ("BOOTIMG", "BOOTIMG.img")):
        raw = next(p["raw"] for p in manifest["payloads"] if p["target_partition"] == partition)
        path = base / filename
        assert sha(path) == raw["sha256"] and path.stat().st_size == raw["size_bytes"], "base identity mismatch"
        identities[partition] = raw
    release = build / "cargo/armv7-unknown-linux-gnueabihf/release"
    bridge = list((release / "build").glob("reborn-media-*/out/libreborn_media.so"))
    assert len(bridge) == 1, "ambiguous media membrane"
    payload = {"usr/bin/" + name: release / name for name in ("reborn", "rebornctl", "reborn-bench")}
    payload["usr/lib/reborn/libreborn_media.so"] = bridge[0]
    elfs = {name: elf(path) for name, path in payload.items()}
    assert source.encode() in payload["usr/bin/reborn"].read_bytes(), "compiled source marker missing"
    renderer_libs = {"libEGL.so.1", "libGLESv2.so.2", "libgbm.so.1", "libdrm.so.2"}
    assert renderer_libs <= set(elfs["usr/bin/reborn"]["needed"]), "hardware renderer dependencies missing"
    assert not any(n.startswith("libav") for n in elfs["usr/bin/reborn"]["needed"]), "FFmpeg must remain lazy"
    assert "libavcodec.so.63" in elfs["usr/lib/reborn/libreborn_media.so"]["needed"]
    for demo in (b"Northark", b"Studio Headphones", b"sample_album_art.png"):
        assert demo not in payload["usr/bin/reborn"].read_bytes(), "preview content in production binary"
    output.mkdir()
    for name in ("fallback", "metadata", "validation", "sources", "licenses"):
        (output / name).mkdir()
    root = output / "Y2ROOT.img"
    for target in (root, output / "fallback/Y2ROOT.img"):
        subprocess.run(["cp", "--reflink=auto", "--sparse=always", str(base / "Y2ROOT.img"), str(target)], check=True)
    versions = json.loads(image_read(root, "/etc/y2linux/versions.json"))
    versions["reborn_version"] = UI_VERSION
    versions["reborn_source_commit"] = source
    versions["reborn_ui_package"] = "Y2LINUX-REBORN-UI-V1-CANDIDATE-01"
    versions["reborn_ui_review_commit"] = run("git", "-C", args.platform, "rev-parse", "HEAD").decode().strip()
    # The platform build, rootfs release, kernel and source identifiers remain
    # unchanged. A distinct application package identifier records this overlay.
    version_file = output / "metadata/versions.json"
    version_file.write_text(json.dumps(versions, indent=2) + "\n")
    payload["etc/y2linux/versions.json"] = version_file
    for name, path in (("font-license.txt", "assets/fonts/DejaVuSans.LICENSE"), ("font-provenance.json", "assets/fonts/provenance.json"), ("icon-provenance.json", "assets/icons/provenance.json")):
        payload["usr/share/reborn/" + name] = REPO / path
    script = output / "metadata/root-overlay.debugfs"
    commands = []
    for target, host in payload.items():
        if b"Inode:" in run("debugfs", "-R", "stat /" + target, root):
            commands.append("rm /" + target)
        commands.extend(["write " + quoted(host) + " /" + target,
                         "set_inode_field /" + target + " mode " + ("0100755" if target in elfs else "0100644"),
                         "set_inode_field /" + target + " uid 0",
                         "set_inode_field /" + target + " gid 0"])
    script.write_text("\n".join(commands) + "\n")
    result = subprocess.run(["debugfs", "-w", "-f", str(script), str(root)], capture_output=True, check=True)
    (output / "validation/debugfs-overlay.log").write_bytes(result.stdout + result.stderr)
    for target, host in payload.items():
        assert image_read(root, "/" + target) == host.read_bytes(), (target, "image readback mismatch")
    unchanged = ["etc/y2linux/build-id", "etc/y2linux/capabilities.json", "etc/init.d/S05reborn", "usr/libexec/reborn-supervise", "usr/bin/y2-platform", "usr/sbin/y2-update-core", "usr/share/reborn/fixtures/tone.flac"]
    for target in unchanged:
        assert image_read(root, "/" + target) == image_read(base / "Y2ROOT.img", "/" + target), target
    header = run("dumpe2fs", "-h", root).decode()
    assert "Y2ROOT" in header and "79324c69-6e75-4801-8000-000000000101" in header
    checked = subprocess.run(["e2fsck", "-fn", str(root)], capture_output=True)
    (output / "validation/e2fsck.log").write_bytes(checked.stdout + checked.stderr)
    assert checked.returncode == 0, "filesystem validation failed"
    assert root.stat().st_size == 536870912
    scatter = root_only((base / "MT6582_preserve_data_scatter.txt").read_text())
    for directory in (output, output / "fallback"):
        (directory / "MT6582_reborn_root_only_scatter.txt").write_text(scatter)
    rootfs = build / "rootfs"
    assert not rootfs.exists(), "fresh extraction required"
    rootfs.mkdir()
    extraction = subprocess.run(["debugfs", "-R", "rdump / " + str(rootfs), str(root)], capture_output=True, check=True)
    (output / "validation/root-extract.log").write_bytes(extraction.stdout + extraction.stderr)
    for target, host in payload.items():
        assert (rootfs / target).read_bytes() == host.read_bytes(), target
    for target in ("usr/bin/reborn-preview", "usr/bin/ffmpeg", "usr/bin/node", "usr/lib/dri/swrast_dri.so"):
        assert not (rootfs / target).exists(), target
    assert not list((rootfs / "data").glob("reborn/*")), "replacement user state"
    assert not list((rootfs / "root").glob(".ssh/*")), "owner SSH material"
    # Resolve dependency existence without following absolute image symlinks on
    # the host. Full loader/decoder validation follows inside the build sandbox.
    for record in elfs.values():
        for dependency in record["needed"]:
            assert any(os.path.lexists(rootfs / folder / dependency) for folder in ("lib", "usr/lib")), dependency
    (build / "buildroot").mkdir(exist_ok=True)
    (build / "buildroot/target").symlink_to("../rootfs")
    for source_file, destination in ((REPO / "Cargo.lock", "Cargo.lock"), (REPO / "assets/fonts/provenance.json", "font-provenance.json"), (REPO / "assets/icons/provenance.json", "icon-provenance.json"), (base / "manifest.json", "base-platform-manifest.json")):
        shutil.copyfile(source_file, output / "metadata" / destination)
    shutil.copyfile(REPO / "assets/fonts/DejaVuSans.LICENSE", output / "licenses/DejaVuSans.LICENSE")
    shutil.copyfile(REPO / "LICENSE", output / "licenses/Reborn.LICENSE")
    shutil.copyfile(build / "cross-build.log", output / "validation/cross-build.log")
    report = {"schema": "org.reborn.ui-candidate/v1", "status": "IMAGE_VALIDATED_PHYSICAL_PENDING",
              "reborn_version": UI_VERSION, "reborn_compiled_commit": source,
              "platform_review_commit": versions["reborn_ui_review_commit"], "platform_versions": versions,
              "root": {"file": "Y2ROOT.img", "bytes": root.stat().st_size, "sha256": sha(root)},
              "fallback": {"file": "fallback/Y2ROOT.img", **{k: v for k, v in identities["ANDROID"].items() if k != "file"}},
              "required_installed_bootimg": identities["BOOTIMG"], "bootimg_payload_included": False,
              "selected_partitions": ["ANDROID"], "data_policy": "Preserve Y2DATA; no data payload, erase or format",
              "elfs": elfs, "replaced_files": list(payload), "critical_platform_files_unchanged": unchanged,
              "physical_qualification": False, "flashed": False}
    (output / "manifest.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"candidate": str(output), "root_sha256": report["root"]["sha256"], "compiled_commit": source}, indent=2))


if __name__ == "__main__":
    main()
