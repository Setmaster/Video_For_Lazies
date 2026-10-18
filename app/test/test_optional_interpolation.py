"""Stdlib-only protocol checks, independent of Torch and downloaded models.

Run directly with Python 3.11+: python -I -B -m unittest discover -s test
-p test_optional_interpolation.py. npm test runs these through the Node bridge.
"""

import contextlib
import errno
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import struct
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock


MODEL_LIBRARIES = {"torch", "safetensors", "numpy", "einops", "diffusers", "transformers", "training"}


class NoModelImports:
    def find_spec(self, fullname, path=None, target=None):
        if fullname.partition(".")[0] in MODEL_LIBRARIES:
            raise ImportError("Protocol tests must not import model libraries: " + fullname)
        return None


sys.meta_path.insert(0, NoModelImports())
SOURCE = Path(__file__).resolve().parent.parent / "optional-interpolation" / "worker.py"
SPEC = importlib.util.spec_from_file_location("vfl_protocol_worker", SOURCE)
worker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(worker)
TORCH_CONTEXT = SimpleNamespace(inference_mode=contextlib.nullcontext)


class PartialReads(io.BytesIO):
    def readinto(self, target):
        return super().readinto(target[:2])


def symlink_or_skip(case, link, target):
    try:
        link.symlink_to(target, target_is_directory=target.is_dir())
    except OSError as error:
        if error.errno in (errno.EPERM, errno.EACCES, errno.ENOSYS) or getattr(error, "winerror", None) == 1314:
            case.skipTest("This OS does not permit creating the test symlink.")
        raise


class ReadAndPairTests(unittest.TestCase):
    def pair_output(self, payload):
        class NoSynthesis:
            def infer(self, *args):
                raise AssertionError("Endpoint requests must bypass model synthesis")

        output = io.BytesIO()
        with mock.patch.object(sys, "stdin", SimpleNamespace(buffer=PartialReads(payload))):
            worker.pair_stream(SimpleNamespace(width=1, height=1), NoSynthesis(), "cpu", TORCH_CONTEXT, output)
        return output.getvalue()

    def test_partial_reads_reassemble(self):
        self.assertEqual(worker.exact_read(PartialReads(b"abcdef"), 6), b"abcdef")

    def test_clean_eof_and_truncated_records_are_distinct(self):
        self.assertIsNone(worker.exact_read(io.BytesIO(), 4, allow_eof=True))
        for allow_eof in (True, False):
            with self.subTest(allow_eof=allow_eof), self.assertRaisesRegex(ValueError, "^truncated-record$"):
                worker.exact_read(io.BytesIO(b"ab"), 4, allow_eof=allow_eof)

    def test_dimension_limits(self):
        self.assertEqual(worker.dimensions(3840, 2160), 24_883_200)
        self.assertEqual(worker.dimensions(8192, 1), 24_576)
        for width, height in ((0, 1), (1, -1), (8193, 1), (3841, 2160)):
            with self.subTest(width=width, height=height), self.assertRaisesRegex(ValueError, "^invalid-dimensions$"):
                worker.dimensions(width, height)

    def test_multiple_endpoint_records_preserve_exact_rgb(self):
        left, right = b"\x00\xff\x7f", b"\x12\x34\x56"
        payload = struct.pack("<f", 0) + left + right + struct.pack("<f", 1) + left + right
        self.assertEqual(self.pair_output(payload), left + right)

    def test_empty_stream(self):
        self.assertEqual(self.pair_output(b""), b"")

    def test_invalid_alpha_cannot_reach_synthesis(self):
        for alpha in (float("nan"), float("inf"), -0.01, 1.01):
            with self.subTest(alpha=alpha), self.assertRaisesRegex(ValueError, "^invalid-alpha$"):
                self.pair_output(struct.pack("<f", alpha) + b"abcdef")

    def test_truncated_alpha_or_endpoint_picture_fails(self):
        for payload in (b"\x00\x00", struct.pack("<f", 0) + b"abcd"):
            with self.subTest(payload=payload), self.assertRaisesRegex(ValueError, "^truncated-record$"):
                self.pair_output(payload)

    def test_worker_does_not_import_model_libraries(self):
        self.assertFalse(MODEL_LIBRARIES.intersection(sys.modules))


class SequenceAdapter:
    def __init__(self, count):
        self.count = count
        self.frames = None

    def infer_sequence(self, frames, factor):
        self.frames = frames
        for index in range(self.count):
            yield ("x%02d" % index).encode()


class SequenceTests(unittest.TestCase):
    def setUp(self):
        location = tempfile.TemporaryDirectory(prefix="vfl-sequence-test-")
        self.addCleanup(location.cleanup)
        self.root = Path(location.name)
        self.raw = self.root / "frames.rgb"
        self.raw.write_bytes(b"redgrnblu")
        self.args = SimpleNamespace(model_id="ldf-vfi", factor=2, frames=self.raw, frame_count=3, width=1, height=1)
        # Byte doubles isolate transport/count behavior from tensor conversion.
        for name, function in (("to_tensor", lambda data, *args: bytes(data)), ("to_bytes", lambda picture, *args: bytes(picture))):
            patch = mock.patch.object(worker, name, function)
            patch.start()
            self.addCleanup(patch.stop)

    def sequence_output(self, adapter, **changes):
        args = SimpleNamespace(**{**vars(self.args), **changes})
        output = io.BytesIO()
        worker.sequence_stream(args, adapter, "cpu", TORCH_CONTEXT, output)
        return output.getvalue()

    def test_source_anchors_and_interior_order(self):
        adapter = SequenceAdapter(5)
        self.assertEqual(self.sequence_output(adapter), b"redx01grnx03blu")
        self.assertTrue(adapter.frames.stream.closed)

    def test_wrong_output_counts_fail_and_close_input(self):
        for count, code in ((4, "incomplete-sequence-output"), (6, "extra-sequence-output")):
            adapter = SequenceAdapter(count)
            with self.subTest(count=count), self.assertRaisesRegex(ValueError, "^" + code + "$"):
                self.sequence_output(adapter)
            self.assertTrue(adapter.frames.stream.closed)

    def test_invalid_mode_and_multiplier(self):
        for changes in ({"factor": 1}, {"factor": 17}, {"model_id": "gmfss-fortuna-union"}, {"frames": None}):
            with self.subTest(changes=changes), self.assertRaisesRegex(ValueError, "^invalid-sequence-request$"):
                self.sequence_output(SequenceAdapter(5), **changes)

    def test_raw_file_size_must_match_frame_count(self):
        self.raw.write_bytes(b"redgrn")
        with self.assertRaisesRegex(ValueError, "^invalid-source-frames$"):
            self.sequence_output(SequenceAdapter(5))

    def test_raw_file_symlink_is_rejected(self):
        link = self.root / "redirect.rgb"
        symlink_or_skip(self, link, self.raw)
        with self.assertRaisesRegex(ValueError, "^invalid-source-frames$"):
            self.sequence_output(SequenceAdapter(5), frames=link)

    def test_input_frame_count_is_bounded(self):
        for count in (0, worker.MAX_FRAMES + 1):
            with self.subTest(count=count), self.assertRaisesRegex(ValueError, "^invalid-frame-count$"):
                self.sequence_output(SequenceAdapter(5), frame_count=count)

    def test_output_frame_count_is_bounded_before_inference(self):
        count = 62_501
        self.raw.write_bytes(b"rgb" * count)
        adapter = SequenceAdapter(0)
        with self.assertRaisesRegex(ValueError, "^invalid-frame-count$"):
            self.sequence_output(adapter, factor=16, frame_count=count)
        self.assertIsNone(adapter.frames)


class ComponentVerificationTests(unittest.TestCase):
    def setUp(self):
        location = tempfile.TemporaryDirectory(prefix="vfl-component-test-")
        self.addCleanup(location.cleanup)
        self.root = Path(location.name)
        self.models, self.sources, scripts = (self.root / name for name in ("models", "sources", "app"))
        for directory in (self.models, self.sources, scripts):
            directory.mkdir()
        self.model, self.code = self.models / "weights.bin", self.sources / "module.py"
        self.model.write_bytes(b"inert weight fixture")
        self.code.write_bytes(b"# inert source fixture\n")
        self.args = SimpleNamespace(model_id="gmfss-fortuna-union", model=self.models, sources=self.sources)
        self.records = [{"path": prefix + "/" + path.name, "bytes": path.stat().st_size,
                         "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
                        for prefix, path in (("models", self.model), ("sources", self.code))]
        self.manifest = scripts / "gmfss-files.json"
        self.write_manifest()
        patch = mock.patch.object(worker, "__file__", str(scripts / "worker.py"))
        patch.start()
        self.addCleanup(patch.stop)

    def write_manifest(self, **changes):
        data = {"schemaVersion": 1, "id": self.args.model_id, "files": self.records, **changes}
        self.manifest.write_text(json.dumps(data), encoding="utf-8")

    def test_exact_inventory_passes(self):
        worker.verify_component(self.args)

    def test_same_size_tampering_fails(self):
        self.model.write_bytes(b"x" * self.records[0]["bytes"])
        with self.assertRaisesRegex(ValueError, "^component-digest-mismatch$"):
            worker.verify_component(self.args)

    def test_wrong_file_size_fails(self):
        self.model.write_bytes(b"short")
        with self.assertRaisesRegex(ValueError, "^component-size-mismatch$"):
            worker.verify_component(self.args)

    def test_unknown_initializer_fails(self):
        (self.sources / "__init__.py").write_text("# unexpected initializer\n", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "^unexpected-component-files$"):
            worker.verify_component(self.args)

    def test_invalid_paths_fail(self):
        for path in ("models/../outside.bin", "other/file", "models\\file", "models//file"):
            self.write_manifest(files=[{**self.records[0], "path": path}, self.records[1]])
            with self.subTest(path=path), self.assertRaisesRegex(ValueError, "^invalid-component-path$"):
                worker.verify_component(self.args)

    def test_source_symlink_fails(self):
        outside = self.root / "outside.py"
        outside.write_bytes(self.code.read_bytes())
        self.code.unlink()
        symlink_or_skip(self, self.code, outside)
        with self.assertRaisesRegex(ValueError, "^redirected-component$"):
            worker.verify_component(self.args)

    def test_manifest_identity_and_schema_fail_closed(self):
        for changes in ({"schemaVersion": 2}, {"id": "ldf-vfi"}):
            self.write_manifest(**changes)
            with self.subTest(changes=changes), self.assertRaisesRegex(ValueError, "^invalid-component-manifest$"):
                worker.verify_component(self.args)


if __name__ == "__main__":
    unittest.main()
