#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Download and extract a pinned official COPT package for CI use."""

from __future__ import annotations

import argparse
import pathlib
import shutil
import tarfile
import urllib.parse
import urllib.request
import zipfile


def download(url: str, destination: pathlib.Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    with urllib.request.urlopen(url) as response, destination.open("wb") as fh:
        shutil.copyfileobj(response, fh)


def extract(archive: pathlib.Path, dest_root: pathlib.Path) -> None:
    extract_root = dest_root / "extract"
    if extract_root.exists():
        shutil.rmtree(extract_root)
    extract_root.mkdir(parents=True, exist_ok=True)

    if archive.name.endswith(".tar.gz"):
        with tarfile.open(archive, "r:gz") as tf:
            tf.extractall(extract_root)
        return
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as zf:
            zf.extractall(extract_root)
        return
    raise ValueError(f"unsupported archive type: {archive.name}")


def find_copt_home(search_root: pathlib.Path) -> pathlib.Path:
    for include_dir in search_root.rglob("include/coptcpp_inc"):
        candidate = include_dir.parent.parent
        if (candidate / "lib").is_dir():
            return candidate.resolve()
    raise FileNotFoundError(
        f"could not find extracted COPT home under {search_root}"
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--url", required=True, help="official COPT package URL")
    parser.add_argument(
        "--dest-root",
        required=True,
        help="directory used to store the downloaded archive and extraction output",
    )
    parser.add_argument(
        "--github-output-name",
        default="copt_home",
        help="when set, print a GitHub Actions output assignment for the located COPT home",
    )
    args = parser.parse_args()

    dest_root = pathlib.Path(args.dest_root).resolve()
    if dest_root.exists():
        shutil.rmtree(dest_root)
    dest_root.mkdir(parents=True, exist_ok=True)

    archive_name = pathlib.Path(urllib.parse.urlparse(args.url).path).name
    archive_path = dest_root / archive_name
    download(args.url, archive_path)
    extract(archive_path, dest_root)
    copt_home = find_copt_home(dest_root / "extract")
    print(f"{args.github_output_name}={copt_home}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
