#!/usr/bin/env python3
"""
Create test directories containing many tiny files.

By default this creates:
  10, 100, 250, 1000, 10000, 100000, 2500000 files

Each file contains exactly one character.
"""

from __future__ import annotations

import argparse
import shutil
import time
from pathlib import Path


DEFAULT_COUNTS = [10, 100, 250, 1000, 10_000, 100_000, 2_500_000]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Generate directories with large numbers of one-character files."
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=Path("test_file_sets"),
        help="Root directory where generated folders will be created (default: ./test_file_sets).",
    )
    parser.add_argument(
        "--char",
        default="x",
        help="Single character written to each file (default: x).",
    )
    parser.add_argument(
        "--overwrite",
        action="store_true",
        help="Delete and recreate output directories if they already exist.",
    )
    return parser.parse_args()


def validate_char(value: str) -> str:
    if len(value) != 1:
        raise ValueError("--char must be exactly one character.")
    return value


def create_files(target_dir: Path, count: int, payload: bytes) -> None:
    target_dir.mkdir(parents=True, exist_ok=True)
    start = time.perf_counter()

    for i in range(count):
        file_path = target_dir / f"f_{i:07d}.txt"
        with file_path.open("wb") as f:
            f.write(payload)

        if i > 0 and i % 100_000 == 0:
            elapsed = time.perf_counter() - start
            print(f"  created {i:,}/{count:,} files in {target_dir.name} ({elapsed:.1f}s)")

    elapsed = time.perf_counter() - start
    print(f"  done {target_dir.name}: {count:,} files ({elapsed:.1f}s)")


def prepare_dir(path: Path, overwrite: bool) -> None:
    if path.exists() and overwrite:
        shutil.rmtree(path)
    path.mkdir(parents=True, exist_ok=True)


def main() -> int:
    args = parse_args()
    char_value = validate_char(args.char)
    payload = char_value.encode("utf-8")

    root: Path = args.root
    prepare_dir(root, overwrite=False)

    print(f"Output root: {root.resolve()}")
    print("Counts:", ", ".join(f"{c:,}" for c in DEFAULT_COUNTS))
    print(f"File payload: {char_value!r}")

    for count in DEFAULT_COUNTS:
        folder = root / f"files_{count}"
        if folder.exists():
            if not args.overwrite:
                print(f"  skipping existing directory: {folder}")
                continue
            shutil.rmtree(folder)
        create_files(folder, count, payload)

    print("All requested directories processed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
