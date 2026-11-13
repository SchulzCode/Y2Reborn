#!/usr/bin/env python3
"""Fetch the headphone profiles Reborn ships (assets/eq/) from AutoEq.

The AutoEq project (MIT, Jaakko Pasanen) publishes ParametricEQ.txt files for
measured headphones. This script takes the models in MODELS at the pinned
commit, prefers oratory1990's measurements, then Crinacle's, then the first
listed source, checks each file against the rules of reborn_core::eq::parse
and writes assets/eq/<model> ParametricEQ.txt plus SOURCES.md.

    python3 tools/assets/fetch_eq_profiles.py
"""
import re
import sys
import urllib.parse
import urllib.request
from pathlib import Path

COMMIT = "7ae0f56d53074872b028649617a22bbb4232feb7"  # AutoEq master, 2025-07-20
RAW = f"https://raw.githubusercontent.com/jaakkopasanen/AutoEq/{COMMIT}/results/"
OUT = Path(__file__).resolve().parents[2] / "assets/eq"

MODELS = [
    # Headphones
    "Sennheiser HD 600", "Sennheiser HD 650", "Sennheiser HD 6XX", "Sennheiser HD 560S",
    "Sennheiser HD 800 S", "Sony WH-1000XM4", "Sony WH-1000XM5", "Sony MDR-7506",
    "Bose QuietComfort 35 II", "Bose QuietComfort 45", "Apple AirPods Max",
    "Audio-Technica ATH-M50x", "Audio-Technica ATH-M40x", "Beyerdynamic DT 770 Pro",
    "Beyerdynamic DT 990 Pro", "AKG K371", "AKG K702", "AKG K240 Studio", "Philips SHP9500",
    "HIFIMAN Sundara", "HIFIMAN Edition XS", "Focal Clear", "Koss Porta Pro",
    # In-ear and earbuds
    "Apple AirPods Pro", "Apple EarPods", "Moondrop Aria", "Moondrop Chu", "Moondrop Chu 2",
    "Moondrop Blessing 2", "Moondrop Starfield", "Truthear x Crinacle Zero",
    "Truthear x Crinacle Zero Blue 2", "Truthear Hexa",
    "7Hz Salnotes Zero", "KZ ZSN Pro X", "Sennheiser IE 200", "Sennheiser IE 600",
    "Sennheiser IE 900", "Shure SE215", "Shure SE535", "Etymotic ER2XR", "Etymotic ER4XR",
    "Tin HiFi T2", "Simgot Audio EW200", "TANGZU Wan'er S.G", "Samsung Galaxy Buds2 Pro",
    "Sony WF-1000XM4", "Sony WF-1000XM5",
]


def fetch(path):
    return urllib.request.urlopen(RAW + path, timeout=30).read()


def problem(text):
    """None when reborn_core::eq::parse takes the file unchanged."""
    bands = 0
    for line in text.splitlines():
        content = line.split("#")[0].strip()
        if not content or re.fullmatch(r"Preamp: -?[\d.]+ dB", content):
            continue
        m = re.fullmatch(r"Filter( \d+)?: (ON|OFF) (PK|LSC|HSC) Fc ([\d.]+) Hz Gain (-?[\d.]+) dB Q ([\d.]+)", content)
        if not m:
            return f"line {content!r}"
        if m[2] == "OFF":
            continue
        fc, gain, q = float(m[4]), float(m[5]), float(m[6])
        if not (20 <= fc <= 20000 and -30 <= gain <= 30 and 0.05 <= q <= 30):
            return f"out of range {content!r}"
        bands += 1
    return None if 0 < bands <= 10 else f"{bands} filters"


def main():
    index = fetch("INDEX.md").decode()
    entries = re.findall(r"- \[([^\]]+)\]\(\./([^)]+)\) by (.+)", index)
    OUT.mkdir(parents=True, exist_ok=True)
    for old in OUT.glob("*ParametricEQ.txt"):
        old.unlink()
    rows = []
    for model in MODELS:
        found = [(path, source) for name, path, source in entries if name == model]
        found.sort(key=lambda e: (not e[1].startswith("oratory1990"), not e[1].startswith("crinacle")))
        if not found:
            sys.exit(f"{model}: not in AutoEq {COMMIT}")
        path, source = found[0]
        leaf = urllib.parse.unquote(path.rsplit("/", 1)[1])
        text = fetch(path + "/" + urllib.parse.quote(leaf + " ParametricEQ.txt")).decode()
        if (why := problem(text)) is not None:
            sys.exit(f"{model}: {why}")
        (OUT / f"{model} ParametricEQ.txt").write_text(text)
        rows.append(f"| {model} | {source} | results/{urllib.parse.unquote(path)} |")
    (OUT / "SOURCES.md").write_text(
        "# Shipped headphone profiles\n\n"
        f"From [AutoEq](https://github.com/jaakkopasanen/AutoEq) commit `{COMMIT}` (MIT, see\n"
        "LICENSE-AutoEq.txt), fetched by `tools/assets/fetch_eq_profiles.py`. Measurements by the sources\n"
        "named; each profile corrects that headphone toward AutoEq's default target.\n\n"
        "| Model | Measurement | AutoEq path |\n| --- | --- | --- |\n" + "\n".join(rows) + "\n")
    (OUT / "LICENSE-AutoEq.txt").write_bytes(
        urllib.request.urlopen(f"https://raw.githubusercontent.com/jaakkopasanen/AutoEq/{COMMIT}/LICENSE",
                               timeout=30).read())
    print(f"{len(rows)} profiles in {OUT}")


if __name__ == "__main__":
    main()
