"""Offline wheel assembly for a hash-verified, privately staged CPython prefix.

The native installer extracts the pinned Python archive first. This helper never
uses pip, resolves dependencies, contacts a server, imports a wheel or runs hooks.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import sys
import zipfile


MAX_ENTRIES = 150_000
MAX_EXPANDED = 18 * 1024**3
MAX_FILE = 4 * 1024**3
CHUNK = 1024 * 1024
COMPILER_WRAPPERS = {
    "bin/vfl-cc": b'#!/bin/sh\nbase=${0%/*}\nexec "$base/python3.11" -I -B "$base/vfl-compiler.py" cc "$@"\n',
    "bin/vfl-cxx": b'#!/bin/sh\nbase=${0%/*}\nexec "$base/python3.11" -I -B "$base/vfl-compiler.py" c++ "$@"\n',
    "bin/vfl-compiler.py": b'''"""Adapt GNU exact-soname library flags for the bundled Zig compiler."""
import os
from pathlib import Path
import sys

def normalize(arguments):
    directories = []
    for index, argument in enumerate(arguments):
        if argument == "-L" and index + 1 < len(arguments):
            directories.append(Path(arguments[index + 1]))
        elif argument.startswith("-L") and len(argument) > 2:
            directories.append(Path(argument[2:]))
    result = []
    for argument in arguments:
        if argument.startswith("-l:"):
            name = argument[3:]
            if not name or name in (".", "..") or "/" in name or "\\\\" in name:
                raise ValueError("Invalid exact library name")
            matches = [directory / name for directory in directories if (directory / name).is_file()]
            if not matches:
                raise ValueError("Exact library is absent from supplied search directories")
            argument = str(matches[0].absolute())
        result.append(argument)
    return result

def main():
    if len(sys.argv) < 2 or sys.argv[1] not in ("cc", "c++"):
        raise ValueError("Unsupported compiler mode")
    compiler = Path(__file__).resolve().parent.parent / "lib/python3.11/site-packages/ziglang/zig"
    os.execv(str(compiler), [str(compiler), sys.argv[1], *normalize(sys.argv[2:])])

if __name__ == "__main__":
    main()
''',
}
SHA256 = re.compile(r"^[a-f0-9]{64}$")
BAD_COMPONENT = re.compile(r"[\x00-\x1f<>:\"|?*]|[ .]$")
WINDOWS_RESERVED = re.compile(r"^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?:\.|$)", re.I)


def fail(message):
    raise ValueError(message)


def sha_file(file):
    digest = hashlib.sha256()
    with file.open("rb") as handle:
        while block := handle.read(CHUNK):
            digest.update(block)
    return digest.hexdigest()


def safe_relative(value):
    if not isinstance(value, str) or not value or "\\" in value:
        fail("Archive path is not canonical")
    path = PurePosixPath(value)
    if path.is_absolute() or str(path) != value or any(
        part in ("", ".", "..") or BAD_COMPONENT.search(part) or WINDOWS_RESERVED.match(part)
        for part in value.split("/")
    ):
        fail("Archive path escapes its owned directory or is platform-ambiguous")
    return path


def regular_directory(path):
    if not path.is_dir() or path.is_symlink():
        fail("Expected an existing regular directory")
    if hasattr(path.lstat(), "st_file_attributes") and path.lstat().st_file_attributes & 0x400:
        fail("Reparse directories are not allowed")


def regular_file(path):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or path.is_symlink():
        fail("Expected a regular file")
    if getattr(info, "st_file_attributes", 0) & 0x400:
        fail("Reparse files are not allowed")
    return info


def owned_destination(root, relative):
    parts = safe_relative(relative).parts
    parent = root
    for part in parts[:-1]:
        parent = parent / part
        if not parent.exists():
            parent.mkdir()
        regular_directory(parent)
    return parent / parts[-1]


def wheel_destination(name, wheel_name, site_packages):
    path = safe_relative(name)
    first, *rest = path.parts
    if first.endswith(".data"):
        if len(rest) < 2:
            fail("Malformed wheel data path")
        scheme, *tail = rest
        if scheme in ("purelib", "platlib"):
            return f"{site_packages}/{'/'.join(tail)}"
        if scheme == "data":
            return "/".join(tail)
        if scheme == "headers":
            return f"include/{wheel_name}/{'/'.join(tail)}"
        # Console entry points are not needed by these library-only runtimes.
        fail("Executable wheel scripts are not accepted")
    return f"{site_packages}/{name}"


def inventory(root, runtime_id, case_sensitive=False):
    files = []
    directories = []
    canonical = set()
    for directory, dirs, names in os.walk(root, followlinks=False):
        for name in dirs:
            folder = Path(directory) / name
            regular_directory(folder)
            relative = folder.relative_to(root).as_posix()
            safe_relative(relative)
            key = relative if case_sensitive else relative.casefold()
            if key in canonical:
                fail("Runtime directory path collision")
            canonical.add(key)
            directories.append(relative)
        for name in names:
            file = Path(directory) / name
            relative = file.relative_to(root).as_posix()
            if relative == "runtime-files.json":
                continue
            safe_relative(relative)
            key = relative if case_sensitive else relative.casefold()
            if key in canonical:
                fail("Case-insensitive runtime path collision")
            canonical.add(key)
            info = regular_file(file)
            files.append({"path": relative, "bytes": info.st_size,
                          "sha256": sha_file(file), "mode": stat.S_IMODE(info.st_mode) & 0o777})
    if len(files) + len(directories) > MAX_ENTRIES or sum(item["bytes"] for item in files) > MAX_EXPANDED:
        fail("Complete runtime exceeds its bounded inventory")
    return {"schema": 1, "runtimeId": runtime_id, "directories": sorted(directories),
            "files": sorted(files, key=lambda item: item["path"])}


def assemble(lock_path, platform, cache, root):
    regular_file(lock_path)
    regular_directory(cache)
    regular_directory(root)
    if (root / "runtime-files.json").exists():
        fail("Assembly requires an unfinished private staging prefix")
    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    if lock.get("schema") != 1 or not re.fullmatch(r"[a-z0-9-]{1,96}", lock.get("id", "")):
        fail("Unsupported runtime lock")
    config = lock["platforms"][platform]
    case_sensitive = platform == "linux-x86_64"
    compiler = config.get("compiler")
    if compiler not in (None, "zig") or (compiler == "zig" and not case_sensitive):
        fail("Unsupported portable compiler")
    site_packages = str(safe_relative(config["sitePackages"]))
    executable = root.joinpath(*safe_relative(config["pythonExecutable"]).parts)
    regular_file(executable)
    # Reserve every native Python path before considering any wheel output.
    occupied = set()
    for directory, dirs, names in os.walk(root, followlinks=False):
        for name in dirs:
            regular_directory(Path(directory) / name)
        for name in names:
            file = Path(directory) / name
            regular_file(file)
            relative = file.relative_to(root).as_posix()
            occupied.add(relative if case_sensitive else relative.casefold())
    wheel_inputs = []
    for item in config["wheels"]:
        if not SHA256.fullmatch(item["sha256"]) or not item["filename"].endswith(".whl"):
            fail("Wheel pin is malformed")
        safe_relative(item["filename"])
        if "/" in item["filename"] or not isinstance(item["bytes"], int) or item["bytes"] <= 0:
            fail("Wheel identity is malformed")
        file = cache / (item["sha256"] + "-" + item["filename"])
        info = regular_file(file)
        if info.st_size != item["bytes"] or sha_file(file) != item["sha256"]:
            fail("A downloaded wheel does not match the pinned bytes")
        wheel_inputs.append((item, file))
    plans = []
    shared_empty_namespace = {}
    expanded = entries = 0
    # Preflight the complete set before writing a single wheel member.
    for item, file in wheel_inputs:
        with zipfile.ZipFile(file) as archive:
            for member in archive.infolist():
                # ZipInfo normalizes Windows separators and truncates at NUL.
                # Reject raw aliases before considering the normalized path.
                if member.orig_filename != member.filename:
                    fail("Wheel archive path is not canonical")
                entries += 1
                if entries > MAX_ENTRIES or member.flag_bits & 1:
                    fail("Wheel entry limit or encryption rule violated")
                unix_mode = member.external_attr >> 16
                kind = stat.S_IFMT(unix_mode)
                if kind not in (0, stat.S_IFREG, stat.S_IFDIR):
                    fail("Wheel links and special files are not accepted")
                if member.is_dir():
                    safe_relative(member.filename.rstrip("/"))
                    continue
                if member.compress_type not in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED):
                    fail("Unsupported wheel compression")
                relative = wheel_destination(member.filename, item["name"], site_packages)
                safe_relative(relative)
                key = relative if case_sensitive else relative.casefold()
                # NVIDIA's separately pinned Linux wheels share this one empty
                # namespace initializer. Never permit a native-prefix overwrite,
                # same-wheel duplicate, nonempty payload or any other collision.
                empty_namespace = (member.filename == "nvidia/__init__.py"
                                   and item["name"].startswith("nvidia-")
                                   and member.file_size == 0 and member.CRC == 0
                                   and not unix_mode & 0o111)
                if key in occupied:
                    owners = shared_empty_namespace.get(key)
                    if empty_namespace and owners is not None and item["name"] not in owners:
                        owners.add(item["name"])
                        continue
                    fail("Wheel output collides with an existing runtime file")
                occupied.add(key)
                if empty_namespace:
                    shared_empty_namespace[key] = {item["name"]}
                expanded += member.file_size
                if member.file_size > MAX_FILE or expanded > MAX_EXPANDED:
                    fail("Wheel expansion exceeds its bound")
                plans.append((file, member.filename, relative, member.file_size, unix_mode & 0o777))
    if compiler == "zig":
        if not any(item["name"] == "ziglang" for item, _ in wheel_inputs):
            fail("Portable compiler wheel is missing")
        for relative in COMPILER_WRAPPERS:
            if relative in occupied:
                fail("Portable compiler wrapper would overwrite runtime content")
            occupied.add(relative)
    # Reopen bounded immutable cache files only after all hashes/paths passed.
    archive = None
    current = None
    try:
        for file, name, relative, expected, mode in plans:
            if file != current:
                if archive:
                    archive.close()
                archive = zipfile.ZipFile(file)
                current = file
            destination = owned_destination(root, relative)
            written = 0
            with archive.open(name) as source, destination.open("xb") as target:
                while block := source.read(CHUNK):
                    written += len(block)
                    if written > expected:
                        fail("Wheel entry expanded beyond its declared size")
                    target.write(block)
            if written != expected:
                fail("Wheel entry length differs")
            # No executable hooks run. Keep only ordinary read/execute bits.
            if os.name != "nt":
                destination.chmod(0o755 if mode & 0o111 else 0o644)
    finally:
        if archive:
            archive.close()
    if compiler == "zig":
        for relative, content in COMPILER_WRAPPERS.items():
            destination = owned_destination(root, relative)
            with destination.open("xb") as target:
                target.write(content)
            destination.chmod(0o755)
    result = inventory(root, lock["id"], case_sensitive)
    destination = root / "runtime-files.json"
    with destination.open("x", encoding="utf-8", newline="\n") as handle:
        json.dump(result, handle, separators=(",", ":"), ensure_ascii=True)
        handle.write("\n")
    return {"schema": 1, "status": "assembled", "runtimeId": lock["id"],
            "files": len(result["files"]), "bytes": sum(item["bytes"] for item in result["files"])}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lock", required=True, type=Path)
    parser.add_argument("--platform", required=True, choices=("windows-x86_64", "linux-x86_64"))
    parser.add_argument("--cache", required=True, type=Path)
    parser.add_argument("--root", required=True, type=Path)
    args = parser.parse_args()
    try:
        result = assemble(args.lock.absolute(), args.platform, args.cache.absolute(), args.root.absolute())
    except (OSError, ValueError, KeyError, TypeError, zipfile.BadZipFile, RuntimeError) as error:
        # Paths and upstream text are intentionally excluded from UI-visible errors.
        print(json.dumps({"schema": 1, "status": "error", "message": "Portable runtime assembly failed.",
                          "kind": type(error).__name__}), flush=True)
        return 1
    print(json.dumps(result, separators=(",", ":")), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
