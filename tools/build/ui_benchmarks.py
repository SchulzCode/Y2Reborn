#!/usr/bin/env python3
"""Exercise the installed library/wheel benchmark using newly allocated scratch only."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--binary", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
parser.add_argument("--qemu-root", type=Path)
args = parser.parse_args()
args.output.mkdir(parents=True, exist_ok=True)
prefix = ["qemu-arm", "-cpu", "cortex-a7", "-L", str(args.qemu_root)] if args.qemu_root else []
for count in (1000, 10000, 20000):
    with tempfile.TemporaryDirectory(prefix="ui-v1-benchmark-") as scratch:
        descriptor = os.open(scratch, os.O_RDONLY | os.O_DIRECTORY)
        try:
            reply = subprocess.run(prefix + [str(args.binary), "--scratch-fd", str(descriptor), "--tracks", str(count)], pass_fds=(descriptor,), capture_output=True, timeout=240, env={**os.environ, "SQLITE_TMPDIR": scratch})
            if reply.returncode:
                (args.output / f"library-{count}-failure.txt").write_bytes(reply.stdout + reply.stderr)
                raise RuntimeError(f"benchmark {count}: {reply.stdout[:4096]!r} {reply.stderr[:4096]!r}")
            result = json.loads(reply.stdout)
            result["qualification"] = "ARM userspace emulation only" if args.qemu_root else "host userspace only"
            result["tracks"] = count
            (args.output / f"library-{count}.json").write_text(json.dumps(result, indent=2) + "\n")
            print(f"{count}: complete", flush=True)
        finally:
            os.close(descriptor)
