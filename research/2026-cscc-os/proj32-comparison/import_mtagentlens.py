#!/usr/bin/env python3
"""Vendor a pinned MtAgentLens snapshot; do not import its Git history or run its code."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path, PurePosixPath
import subprocess
import tempfile

UPSTREAM = "https://github.com/LiuZhouKang/MtAgentLens.git"
REVISION = "6465fef610fd7b92dc5c69fcfbc5eab42e969879"
DESTINATION = Path("research/2026-cscc-os/proj32-comparison/sources/01-mtagentlens")
MANIFEST = Path("research/2026-cscc-os/proj32-comparison/MTAGENTLENS_SNAPSHOT.json")


def git(repo: Path | None, *args: str, data: bytes | None = None) -> bytes:
    cmd = ["git", "-c", "core.autocrlf=false", "-c", "core.hooksPath=/dev/null",
           "-c", "protocol.file.allow=never"]
    if repo is not None:
        cmd += ["-C", str(repo)]
    env = dict(os.environ, GIT_TERMINAL_PROMPT="0", GIT_LFS_SKIP_SMUDGE="1")
    return subprocess.run(cmd + list(args), input=data, stdout=subprocess.PIPE,
                          check=True, env=env).stdout


def export_tree(source: Path, destination: Path, prefix: str = "") -> tuple[list[dict], list[dict]]:
    """Read tracked Git blobs, preserving bytes/modes and recursively flattening gitlinks."""
    files: list[dict] = []
    modules: list[dict] = []
    for record in git(source, "ls-tree", "-rz", "--full-tree", "HEAD").split(b"\0"):
        if not record:
            continue
        metadata, raw_path = record.split(b"\t", 1)
        mode, kind, sha = metadata.decode("ascii").split()
        relative = os.fsdecode(raw_path)
        parts = PurePosixPath(relative)
        if parts.is_absolute() or ".." in parts.parts or any(p.lower() == ".git" for p in parts.parts):
            raise RuntimeError(f"Unsafe source path: {relative!r}")
        name = prefix + relative
        if kind == "commit":
            module = source / relative
            actual = git(module, "rev-parse", "HEAD").decode().strip()
            if actual != sha:
                raise RuntimeError(f"Submodule is not at its pinned revision: {name}")
            modules.append({"path": name, "commit": sha,
                            "url": git(module, "remote", "get-url", "origin").decode().strip()})
            child_files, child_modules = export_tree(module, destination, name + "/")
            files.extend(child_files)
            modules.extend(child_modules)
            continue
        if kind != "blob" or mode not in {"100644", "100755", "120000"}:
            raise RuntimeError(f"Unsupported Git object: {name} ({mode}, {kind})")
        content = git(source, "cat-file", "blob", sha)
        if content.startswith(b"version https://git-lfs.github.com/spec/v1\n"):
            raise RuntimeError(f"Unresolved Git LFS object: {name}; not claiming a complete import")
        if len(content) >= 100_000_000:
            raise RuntimeError(f"Large file requires explicit handling, not silent omission: {name}")
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True)
        if not target.parent.resolve().is_relative_to(destination.resolve()):
            raise RuntimeError(f"Path traverses an external symlink: {name}")
        if target.exists() or target.is_symlink():
            raise RuntimeError(f"Refusing to overwrite snapshot path: {name}")
        if mode == "120000":
            link = os.fsdecode(content)
            if os.path.isabs(link) or not (target.parent / link).resolve().is_relative_to(destination.resolve()):
                raise RuntimeError(f"External symlink requires explicit review: {name}")
            target.symlink_to(link)
        else:
            target.write_bytes(content)
            target.chmod(0o755 if mode == "100755" else 0o644)
        files.append({"path": name, "mode": mode, "git_blob_sha": sha, "bytes": len(content)})
    return files, modules


def stage_exact(repo: Path, destination: Path, files: list[dict]) -> None:
    """Bypass ignore/attribute filters and prove that every staged blob matches upstream."""
    entries = bytearray()
    for item in files:
        path = destination / item["path"]
        content = os.fsencode(os.readlink(path)) if item["mode"] == "120000" else path.read_bytes()
        sha = git(repo, "hash-object", "-w", "--stdin", data=content).decode().strip()
        if sha != item["git_blob_sha"]:
            raise RuntimeError(f"Blob integrity mismatch: {path}")
        relative = path.relative_to(repo).as_posix()
        entries += f'{item["mode"]} {sha}\t'.encode() + os.fsencode(relative) + b"\0"
    git(repo, "update-index", "-z", "--index-info", data=bytes(entries))
    for record in git(repo, "ls-files", "--stage", "-z", "--", str(destination.relative_to(repo))).split(b"\0"):
        if record.startswith(b"160000 "):
            raise RuntimeError("An unflattened submodule remains in the destination")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path.cwd(), help="Existing operating-systems-lab checkout")
    args = parser.parse_args()
    repo = args.repo.resolve()
    root = Path(git(repo, "rev-parse", "--show-toplevel").decode().strip()).resolve()
    if root != repo:
        raise RuntimeError("--repo must be the repository root")
    if git(repo, "status", "--porcelain").strip():
        raise RuntimeError("Destination checkout must be clean; existing work is never discarded")
    destination = repo / DESTINATION
    if destination.exists() or destination.is_symlink():
        raise RuntimeError(f"Destination already exists; refusing to overwrite: {destination}")
    with tempfile.TemporaryDirectory(prefix="mtagentlens-import-") as tmp:
        source = Path(tmp) / "upstream"
        git(None, "clone", "--no-checkout", "--depth", "1", UPSTREAM, str(source))
        git(source, "fetch", "--depth", "1", "origin", REVISION)
        git(source, "checkout", "--detach", REVISION)
        git(source, "submodule", "update", "--init", "--recursive", "--depth", "1")
        actual = git(source, "rev-parse", "HEAD").decode().strip()
        if actual != REVISION:
            raise RuntimeError("Upstream revision mismatch")
        destination.mkdir(parents=True)
        files, modules = export_tree(source, destination)
        if not files:
            raise RuntimeError("Empty snapshot; import not successful")
        if any(p.name.lower() == ".git" for p in destination.rglob("*")):
            raise RuntimeError("Unexpected Git metadata inside the snapshot")
        manifest = {
            "source": UPSTREAM, "commit": actual,
            "source_tree": git(source, "rev-parse", "HEAD^{tree}").decode().strip(),
            "destination": DESTINATION.as_posix(), "upstream_history_imported": False,
            "lab_history_rewritten": False, "submodules_flattened": modules,
            "file_count": len(files), "total_bytes": sum(f["bytes"] for f in files),
            "files": sorted(files, key=lambda f: f["path"]),
            "validation": "All tracked blobs, executable modes and symlinks preserved; Git metadata omitted.",
            "runtime_validation": "not run: upstream project code is not executed by this importer",
            "attribution": "Original README, LICENSE and NOTICE files are preserved when present; original authors retain copyright."
        }
        manifest_path = repo / MANIFEST
        manifest_path.write_text(json.dumps(manifest, ensure_ascii=True, indent=2) + "\n", encoding="utf-8")
        stage_exact(repo, destination, files)
        git(repo, "add", "--", str(MANIFEST))
        status_path = repo / "research/2026-cscc-os/proj32-comparison/IMPORT_STATUS.md"
        previous = status_path.read_text(encoding="utf-8") if status_path.exists() else ""
        latest = ("# GitHub snapshot import result\n\n"
                  f"- Imported: `LiuZhouKang/MtAgentLens` at `{REVISION}`.\n"
                  f"- Destination: `{DESTINATION.as_posix()}`.\n"
                  f"- Verified tracked files: {len(files)}; flattened submodules: {len(modules)}.\n"
                  "- No upstream Git history imported. Existing lab history retained.\n"
                  "- Source code, original documents and licenses preserved byte-for-byte.\n"
                  "- Project compilation/runtime tests: NOT RUN.\n"
                  "- Other candidate projects: not imported by this run.\n\n"
                  "Detailed file hashes and source revisions: `MTAGENTLENS_SNAPSHOT.json`.\n\n"
                  "## Previous import record (historical)\n\n" + previous)
        status_path.write_text(latest, encoding="utf-8")
        git(repo, "add", "--", str(status_path.relative_to(repo)))
        print(f"STAGED: {len(files)} verified files; {len(modules)} flattened submodules.")
        print("No commit or push performed by this script. Review the staged snapshot before publishing.")


if __name__ == "__main__":
    main()
