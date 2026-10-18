"""Offline optional inference process. VFL owns clocks, cuts and publication."""

import argparse
import contextlib
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import struct
import sys


PROTOCOL = 1
BACKENDS = {
    "gmfss-fortuna-union": "gmfss",
    "gimm-vfi-r-p": "gimm",
    "ldf-vfi": "ldf",
}
MAX_PIXELS = 3840 * 2160
MAX_FRAMES = 1_000_000


def exact_read(stream, length, allow_eof=False):
    chunks = bytearray(length)
    view = memoryview(chunks)
    offset = 0
    while offset < length:
        count = stream.readinto(view[offset:])
        if not count:
            if allow_eof and offset == 0:
                return None
            raise ValueError("truncated-record")
        offset += count
    return chunks


def dimensions(width, height):
    if width < 1 or height < 1 or max(width, height) > 8192:
        raise ValueError("invalid-dimensions")
    if width * height > MAX_PIXELS:
        raise ValueError("invalid-dimensions")
    return width * height * 3


def configure_cache(path):
    path = path.resolve()
    path.mkdir(parents=True, exist_ok=True)
    for name, leaf in {
        "TORCH_HOME": "torch",
        "TORCHINDUCTOR_CACHE_DIR": "inductor",
        "TRITON_CACHE_DIR": "triton",
        "CUDA_CACHE_PATH": "cuda",
        "HF_HOME": "huggingface",
        "ZIG_GLOBAL_CACHE_DIR": "zig-global",
        "ZIG_LOCAL_CACHE_DIR": "zig-local",
    }.items():
        os.environ[name] = str(path / leaf)
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["TRANSFORMERS_OFFLINE"] = "1"
    os.environ["HF_HUB_DISABLE_TELEMETRY"] = "1"
    os.environ["DO_NOT_TRACK"] = "1"
    # Inductor uses getpass.getuser() for its private cache key on Windows.
    # A fixed identity avoids falling back to the unavailable Unix pwd module.
    os.environ["USERNAME"] = "vfl-runtime"
    os.environ["USER"] = "vfl-runtime"
    os.environ["PYTHONDONTWRITEBYTECODE"] = "1"
    sys.dont_write_bytecode = True
    if sys.platform == "linux":
        binary_dir = Path(sys.executable).resolve().parent
        for variable, leaf in (("CC", "vfl-cc"), ("CXX", "vfl-cxx")):
            compiler = binary_dir / leaf
            if compiler.is_file() and not compiler.is_symlink():
                os.environ[variable] = str(compiler)


def load_adapter(args, torch):
    if torch.__version__.split("+", 1)[0] != "2.10.0":
        raise ValueError("incompatible-runtime")
    if args.model_id == "ldf-vfi" and sys.platform == "linux":
        compiler = Path(sys.executable).resolve().parent / "vfl-cc"
        if not compiler.is_file() or compiler.is_symlink() or os.environ.get("CC") != str(compiler):
            raise ValueError("portable-compiler-unavailable")
    if args.gpu == "-1":
        if args.model_id == "ldf-vfi":
            raise ValueError("cuda-required")
        device = "cpu"
    else:
        if not torch.cuda.is_available():
            raise ValueError("cuda-unavailable")
        index = 0 if args.gpu == "auto" else int(args.gpu)
        if index < 0 or index >= torch.cuda.device_count():
            raise ValueError("invalid-device")
        torch.cuda.set_device(index)
        device = f"cuda:{index}"
        if args.model_id == "ldf-vfi" and not torch.cuda.is_bf16_supported():
            raise ValueError("bf16-required")
    verify_component(args)
    module_path = Path(__file__).resolve().parent / f"{BACKENDS[args.model_id]}.py"
    spec = importlib.util.spec_from_file_location("vfl_optional_adapter", module_path)
    if spec is None or spec.loader is None:
        raise ValueError("adapter-unavailable")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    adapter = module.create(args.model.resolve(), args.sources.resolve(), device)
    name = "CPU qualification" if device == "cpu" else torch.cuda.get_device_name()
    return adapter, device, name


def verify_component(args):
    """Standalone launches retain the installer's exact-source execution gate."""
    manifest_path = Path(__file__).resolve().parent / f"{BACKENDS[args.model_id]}-files.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if manifest.get("schemaVersion") != 1 or manifest.get("id") != args.model_id:
        raise ValueError("invalid-component-manifest")
    roots = {"models": args.model, "sources": args.sources,
             "licenses": args.model.parent / "licenses"}
    expected = {key: set() for key in roots}
    for record in manifest["files"]:
        relative = record["path"]
        parts = relative.split("/")
        if (len(parts) < 2 or parts[0] not in roots or "\\" in relative
                or any(part in ("", ".", "..") for part in parts)):
            raise ValueError("invalid-component-path")
        root = roots[parts[0]].resolve(strict=True)
        path = roots[parts[0]]
        for part in parts[1:]:
            path = path / part
            if path.is_symlink():
                raise ValueError("redirected-component")
        if path.resolve(strict=True).parent != root and not path.resolve(strict=True).is_relative_to(root):
            raise ValueError("redirected-component")
        if not path.is_file() or path.stat().st_size != record["bytes"]:
            raise ValueError("component-size-mismatch")
        with path.open("rb") as stream:
            actual = hashlib.file_digest(stream, "sha256").hexdigest()
        if actual != record["sha256"]:
            raise ValueError("component-digest-mismatch")
        expected[parts[0]].add("/".join(parts[1:]))
    for kind in ("models", "sources"):
        root = roots[kind]
        if root.is_symlink():
            raise ValueError("redirected-component")
        found = set()
        for path in root.rglob("*"):
            if path.is_symlink():
                raise ValueError("redirected-component")
            if path.is_file():
                found.add(path.relative_to(root).as_posix())
            elif not path.is_dir():
                raise ValueError("invalid-component-entry")
        if found != expected[kind]:
            raise ValueError("unexpected-component-files")


def to_tensor(data, width, height, device, torch):
    # frombuffer borrows the bytearray only until the explicit device/dtype copy.
    return torch.frombuffer(data, dtype=torch.uint8).reshape(height, width, 3).permute(
        2, 0, 1
    ).unsqueeze(0).to(device=device, dtype=torch.float32).div_(255.0)


def to_bytes(picture, width, height, torch):
    if tuple(picture.shape) != (1, 3, height, width):
        raise ValueError("invalid-output-shape")
    if not bool(torch.isfinite(picture).all().item()):
        raise ValueError("nonfinite-output")
    return picture[0].detach().clamp(0, 1).mul(255).round().to(
        dtype=torch.uint8, device="cpu"
    ).permute(1, 2, 0).contiguous().numpy().tobytes()


class RawFrames:
    """Seekable bounded input: only the selected frame is materialized."""

    def __init__(self, path, count, width, height, device, torch):
        self.bytes = dimensions(width, height)
        if not 1 <= count <= MAX_FRAMES:
            raise ValueError("invalid-frame-count")
        if path.is_symlink() or not path.is_file() or path.stat().st_size != count * self.bytes:
            raise ValueError("invalid-source-frames")
        self.stream = path.open("rb")
        self.count, self.width, self.height = count, width, height
        self.device, self.torch = device, torch

    def __len__(self):
        return self.count

    def __getitem__(self, index):
        if not isinstance(index, int) or not 0 <= index < self.count:
            raise IndexError(index)
        self.stream.seek(index * self.bytes)
        return to_tensor(exact_read(self.stream, self.bytes), self.width, self.height,
                         self.device, self.torch)

    def close(self):
        self.stream.close()


def pair_stream(args, adapter, device, torch, output):
    size = dimensions(args.width, args.height)
    source = sys.stdin.buffer
    with torch.inference_mode():
        while True:
            raw_alpha = exact_read(source, 4, allow_eof=True)
            if raw_alpha is None:
                break
            alpha = struct.unpack("<f", raw_alpha)[0]
            if not math.isfinite(alpha) or not 0 <= alpha <= 1:
                raise ValueError("invalid-alpha")
            left = exact_read(source, size)
            right = exact_read(source, size)
            if alpha in (0.0, 1.0):
                result = left if alpha == 0 else right
            else:
                result = to_bytes(adapter.infer(
                    to_tensor(left, args.width, args.height, device, torch),
                    to_tensor(right, args.width, args.height, device, torch), alpha
                ), args.width, args.height, torch)
            output.write(result)
            output.flush()


def sequence_stream(args, adapter, device, torch, output):
    if args.model_id != "ldf-vfi" or not 2 <= args.factor <= 16 or args.frames is None:
        raise ValueError("invalid-sequence-request")
    frames = RawFrames(args.frames, args.frame_count, args.width, args.height, device, torch)
    expected = (len(frames) - 1) * args.factor + 1
    if expected > MAX_FRAMES:
        frames.close()
        raise ValueError("invalid-frame-count")
    count = 0
    try:
        with torch.inference_mode():
            for picture in adapter.infer_sequence(frames, args.factor):
                if count >= expected:
                    raise ValueError("extra-sequence-output")
                # Restore source anchors exactly. The model never owns source timing.
                if count % args.factor == 0:
                    picture = frames[count // args.factor]
                output.write(to_bytes(picture, args.width, args.height, torch))
                output.flush()
                count += 1
        if count != expected:
            raise ValueError("incomplete-sequence-output")
    finally:
        frames.close()


def self_test(args, adapter, device, name, torch, output):
    width, height = 129, 131
    grid = torch.linspace(0, 1, width * height, device=device).reshape(1, 1, height, width)
    left = torch.cat((grid, grid.flip(-1), grid.flip(-2)), dim=1)
    right = torch.roll(left, shifts=4, dims=-1).mul(0.8)
    with torch.inference_mode():
        if args.model_id == "ldf-vfi":
            count = 0
            synthesized = False
            for picture in adapter.infer_sequence([left, right], 2):
                to_bytes(picture, width, height, torch)
                if count == 1:
                    middle = picture.to(device=device)
                    synthesized = (float(middle.std().item()) > 1e-4
                                   and float((middle - left).abs().max().item()) > 1e-4
                                   and float((middle - right).abs().max().item()) > 1e-4)
                count += 1
            if count != 3 or not synthesized:
                raise ValueError("self-test-frame-count")
        else:
            first = adapter.infer(left, right, 0.25)
            second = adapter.infer(left, right, 0.75)
            to_bytes(first, width, height, torch)
            to_bytes(second, width, height, torch)
            if float((first - second).abs().max().item()) < 1e-5:
                raise ValueError("self-test-static-output")
    result = {"protocol": PROTOCOL, "ok": True, "deviceName": name,
              "modelId": args.model_id, "backend": BACKENDS[args.model_id]}
    output.write((json.dumps(result) + "\n").encode())
    output.flush()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model-id", choices=BACKENDS, required=True)
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--sources", type=Path, required=True)
    parser.add_argument("--cache", type=Path, required=True)
    parser.add_argument("--gpu", default="auto")
    modes = parser.add_mutually_exclusive_group(required=True)
    modes.add_argument("--self-test", action="store_true")
    modes.add_argument("--stream", action="store_true")
    modes.add_argument("--sequence", action="store_true")
    parser.add_argument("--width", type=int, default=0)
    parser.add_argument("--height", type=int, default=0)
    parser.add_argument("--frames", type=Path)
    parser.add_argument("--frame-count", type=int, default=0)
    parser.add_argument("--factor", type=int, default=0)
    args = parser.parse_args()
    configure_cache(args.cache)
    binary_output = sys.stdout.buffer
    # Third-party initialization messages must never corrupt the binary protocol.
    with contextlib.redirect_stdout(sys.stderr):
        import torch
        torch.set_num_threads(2)
        adapter, device, name = load_adapter(args, torch)
        if args.self_test:
            self_test(args, adapter, device, name, torch, binary_output)
        elif args.stream and args.model_id != "ldf-vfi":
            pair_stream(args, adapter, device, torch, binary_output)
        elif args.sequence:
            sequence_stream(args, adapter, device, torch, binary_output)
        else:
            raise ValueError("incompatible-protocol")


if __name__ == "__main__":
    try:
        main()
    except (Exception, KeyboardInterrupt) as error:
        # Deliberately omit exception reprs: they can include local paths or URLs.
        code = str(error) if isinstance(error, ValueError) and str(error).replace("-", "").isalnum() else "optional-inference-failed"
        print(code, file=sys.stderr)
        raise SystemExit(2) from None
