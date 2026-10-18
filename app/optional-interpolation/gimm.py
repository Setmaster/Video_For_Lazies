"""GIMM-VFI-R-P adapter for the isolated optional-model worker.

Model source and weights remain a separately installed noncommercial S-Lab
component. SOURCE_FILES identifies the reviewed upstream source; this adapter
never imports upstream package initializers, video drivers, or softsplat code.
The forward splat below implements bilinear accumulation with standard Torch
operations, independently of the academic-only custom CUDA implementation.
"""

from __future__ import annotations

import copy
import hashlib
import math
from pathlib import Path
import sys
from types import ModuleType, SimpleNamespace
import uuid


MODEL_ID = "gimm-vfi-r-p"
SOURCE_COMMIT = "4c9a3123762af85e7c796e41737da0b70c75d72d"
SOURCE_BASE_URL = (
    "https://raw.githubusercontent.com/kijai/ComfyUI-GIMM-VFI/"
    + SOURCE_COMMIT
    + "/"
)
# Raw byte sizes and SHA256s, before any in-memory compatibility changes.
SOURCE_FILES = {
    "gimmvfi/generalizable_INR/gimmvfi_r.py": (17989, "c677245baaba247ea8017c5d25fd6c472d90c638c4115edf8e12d4bfaccc9ab2"),
    "gimmvfi/generalizable_INR/modules/coord_sampler.py": (3200, "e8110a024d807f97c61b04a6a8ecfaa03e3b78a0fdea510969b7aecc1daf8347"),
    "gimmvfi/generalizable_INR/modules/hyponet.py": (7388, "2854ada7f95d83507f4ba51bea24476166748436c7e3fc2074e0c1304ed17e70"),
    "gimmvfi/generalizable_INR/modules/fi_components.py": (10978, "dac5c59e55208c7ecff0c42174f1f73c9125cc53ac0e92f4e815ce127e87c5b3"),
    "gimmvfi/generalizable_INR/modules/fi_utils.py": (2460, "e7329e4de89cbea0c4bc9134fde5ebce5f7c64cd34dcb48cae08c6078c2c399a"),
    "gimmvfi/generalizable_INR/modules/utils.py": (2407, "890d23ff91d0ef523f6414c30acf3a995bd184338f4448f21c39779d5e9b91e1"),
    "gimmvfi/generalizable_INR/modules/layers.py": (1021, "03b84709af4ca3687c408c917070a15ada43072fe0cabe60db31e48165a2992d"),
    "gimmvfi/generalizable_INR/raft/raft.py": (5335, "6016d9f74f096b4f6f191d9d75599919179b66697f4d3a5cbb44c3229919feeb"),
    "gimmvfi/generalizable_INR/raft/update.py": (5437, "de0964a489fb96c1a2e401551ca106056b6e8748d3798d610b26e5e1d8987c39"),
    "gimmvfi/generalizable_INR/raft/extractor.py": (9845, "00ab27d368d1c266c812913b340b0b793fba7781bb53d127ac00f6cbb19b67e1"),
    "gimmvfi/generalizable_INR/raft/corr.py": (6278, "d9a9368e4d010cc3b5c6c22a7aff6ba62dcfd3e12b009ae2247c526e3489a62e"),
    "gimmvfi/generalizable_INR/raft/utils/utils.py": (2619, "eddced877ae0f4e5bc80caedff987a01e65071070c3ea2b79e982a3e2a2c02b7"),
}
MODEL_FILES = {
    "gimmvfi_r_arb_lpips_fp32.safetensors": (79204556, "897b892e7ee481f2e5c09e59661b645abbd6c9aba9c0c01346bbe31aa569e277"),
    "raft-things_fp32.safetensors": (21062368, "ba7cb7781f9f67030e2f6acc4151ac8701b633d93d39e8d61991f2b472a0e45a"),
}
MODEL_BASE_URL = (
    "https://huggingface.co/Kijai/GIMM-VFI_safetensors/resolve/"
    "c1089661cf9ac6dcc32fa375ff698f06b3412aff/"
)


class _Config(SimpleNamespace):
    def copy(self):
        return copy.deepcopy(self)


def _config():
    # Pinned R model YAML plus HypoNetConfig/GIMMVFIConfig defaults. Training
    # configuration and arbitrary YAML are intentionally outside the worker.
    return _Config(
        fwarp_type="linear",
        coord_range=[-1.0, 1.0],
        hyponet=_Config(
            use_bias=True,
            n_layer=5,
            hidden_dim=[128],
            input_dim=3,
            output_dim=2,
            output_bias=0.5,
            normalize_weight=True,
            activation=_Config(type="siren", siren_w0=1.0),
            initialization=_Config(weight_init_type="siren", bias_init_type="siren"),
        ),
    )


def _replace_once(source, old, new):
    if source.count(old) != 1:
        raise RuntimeError("GIMM source compatibility patch did not match exactly")
    return source.replace(old, new, 1)


def _remove_section(source, start, end):
    if source.count(start) != 1 or source.count(end) != 1:
        raise RuntimeError("GIMM source section did not match exactly")
    left = source.index(start)
    right = source.index(end, left)
    return source[:left] + source[right:]


def _patch_source(relative, source):
    if relative == "modules/hyponet.py":
        source = _replace_once(source, "from omegaconf import OmegaConf\n", "")
        source = _replace_once(
            source, "OmegaConf.to_object(self.hidden_dims)", "list(self.hidden_dims)"
        )
    elif relative == "raft/utils/utils.py":
        source = _replace_once(source, "import numpy as np\n", "")
        source = _replace_once(source, "from scipy import interpolate\n", "")
        source = _remove_section(
            source, "def forward_interpolate(flow):", "def bilinear_sampler("
        )
    elif relative == "raft/corr.py":
        source = _replace_once(
            source,
            "try:\n    import alt_cuda_corr\nexcept:\n    # alt_cuda_corr is not compiled\n    pass\n",
            "",
        )
        source = _remove_section(source, "class AlternateCorrBlock:", "class CorrBlock:")
    elif relative == "raft/raft.py":
        source = _replace_once(source, "import numpy as np\n", "")
        source = _replace_once(
            source,
            "from .corr import CorrBlock, AlternateCorrBlock",
            "from .corr import CorrBlock",
        )
        source = _replace_once(
            source,
            "        if self.args.alternate_corr:\n"
            "            corr_fn = AlternateCorrBlock(fmap1, fmap2, radius=self.args.corr_radius)\n"
            "        else:\n"
            "            corr_fn = CorrBlock(fmap1, fmap2, radius=self.args.corr_radius)",
            "        corr_fn = CorrBlock(fmap1, fmap2, radius=self.args.corr_radius)",
        )
    elif relative == "gimmvfi_r.py":
        source = _replace_once(source, "from .raft import initialize_RAFT\n", "")
    elif relative == "modules/fi_utils.py":
        source = _replace_once(
            source, 'device = torch.device("cuda" if torch.cuda.is_available() else "cpu")\n', ""
        )
        source = _replace_once(
            source,
            "def warp(tenInput, tenFlow):\n",
            "def warp(tenInput, tenFlow):\n    device = tenInput.device\n",
        )
        source = _replace_once(
            source,
            "    flows = flows / flow_scaler  # [-1,1]",
            "    flows = flows / flow_scaler.clamp_min(torch.finfo(flows.dtype).tiny)  # VFL: zero flow remains finite",
        )
    return source


def _splat(features, flow, metric, mode):
    """Independent weighted bilinear forward accumulation, no custom kernel.

    Source samples contribute to four neighbors of their destination position.
    The last accumulator channel records the same weights used for features.
    """
    import torch

    if mode != "linear-zeroeps":
        raise ValueError("GIMM adapter only supports linear-zeroeps splatting")
    if features.ndim != 4:
        raise ValueError("Invalid splat feature shape")
    batch, channels, height, width = features.shape
    if flow.shape != (batch, 2, height, width) or metric.shape != (batch, 1, height, width):
        raise ValueError("Invalid splat flow or metric shape")
    if flow.device != features.device or metric.device != features.device:
        raise ValueError("Splat tensors must share a device")
    if not bool(torch.isfinite(flow).all()):
        raise ValueError("Nonfinite GIMM optical flow")
    features, flow, metric = features.float(), flow.float(), metric.float()
    y, x = torch.meshgrid(
        torch.arange(height, device=features.device, dtype=torch.float32),
        torch.arange(width, device=features.device, dtype=torch.float32),
        indexing="ij",
    )
    dest_x, dest_y = x + flow[:, 0], y + flow[:, 1]
    floor_x, floor_y = dest_x.floor(), dest_y.floor()
    frac_x, frac_y = dest_x - floor_x, dest_y - floor_y
    weighted = torch.cat((features * metric, metric), dim=1).flatten(2)
    accumulated = features.new_zeros((batch, channels + 1, height * width))
    for dx, dy in ((0, 0), (1, 0), (0, 1), (1, 1)):
        target_x, target_y = floor_x + dx, floor_y + dy
        valid = (target_x >= 0) & (target_x < width) & (target_y >= 0) & (target_y < height)
        bilinear = (frac_x if dx else 1 - frac_x) * (frac_y if dy else 1 - frac_y)
        contribution = (bilinear * valid).flatten(1).unsqueeze(1)
        index = (target_y.clamp(0, height - 1).long() * width + target_x.clamp(0, width - 1).long()).flatten(1)
        accumulated.scatter_add_(2, index.unsqueeze(1).expand(-1, channels + 1, -1), weighted * contribution)
    accumulated = accumulated.reshape(batch, channels + 1, height, width)
    numerator, denominator = accumulated[:, :-1], accumulated[:, -1:]
    return numerator / torch.where(denominator == 0, torch.ones_like(denominator), denominator)


def _softsplat(*, tenIn, tenFlow, tenMetric, strMode):
    # Compatibility signature only; the operation above is independent code.
    return _splat(tenIn, tenFlow, tenMetric, strMode)


def _checked_bytes(root, relative, expected):
    root = Path(root).resolve(strict=True)
    path = (root / relative).resolve(strict=True)
    if not path.is_relative_to(root) or not path.is_file():
        raise RuntimeError("GIMM file is outside its installed component")
    size, digest = expected
    if path.stat().st_size != size:
        raise RuntimeError("GIMM file size verification failed")
    data = path.read_bytes()
    if len(data) != size or hashlib.sha256(data).hexdigest() != digest:
        raise RuntimeError("GIMM file digest verification failed")
    return data


def _load_source(sourcedir):
    # Verify the entire closure before executing any source. Packages have no
    # filesystem search path, preventing an unreviewed initializer/helper import.
    checked = {
        name.removeprefix("gimmvfi/generalizable_INR/"): _checked_bytes(sourcedir, name, spec).decode("utf-8")
        for name, spec in SOURCE_FILES.items()
    }
    prefix = "_vfl_gimm_" + uuid.uuid4().hex

    def package(suffix):
        name = prefix + suffix
        module = ModuleType(name)
        module.__package__ = name
        module.__path__ = []
        sys.modules[name] = module
        return module

    package("")
    for suffix in (".modules", ".raft", ".raft.utils"):
        package(suffix)
    config_module = ModuleType(prefix + ".configs")
    config_module.GIMMVFIConfig = _Config
    config_module.HypoNetConfig = _Config
    sys.modules[config_module.__name__] = config_module
    splat_module = ModuleType(prefix + ".modules.softsplat")
    splat_module.softsplat = _softsplat
    sys.modules[splat_module.__name__] = splat_module
    order = (
        "modules/layers.py", "modules/utils.py", "modules/hyponet.py",
        "modules/coord_sampler.py", "modules/fi_utils.py", "modules/fi_components.py",
        "raft/utils/utils.py", "raft/corr.py", "raft/update.py", "raft/extractor.py",
        "raft/raft.py", "gimmvfi_r.py",
    )
    try:
        for relative in order:
            name = prefix + "." + relative[:-3].replace("/", ".")
            module = ModuleType(name)
            module.__package__ = name.rpartition(".")[0]
            module.__file__ = str(Path(sourcedir) / "gimmvfi/generalizable_INR" / relative)
            source = _patch_source(relative, checked[relative])
            sys.modules[name] = module
            exec(compile(source, module.__file__, "exec"), module.__dict__)
        return sys.modules[prefix + ".gimmvfi_r"].GIMMVFI_R, sys.modules[prefix + ".raft.raft"].RAFT
    except BaseException:
        for name in tuple(sys.modules):
            if name == prefix or name.startswith(prefix + "."):
                del sys.modules[name]
        raise


class _RaftArgs(SimpleNamespace):
    def _get_kwargs(self):
        return vars(self)


class Adapter:
    def __init__(self, model, device):
        self.model = model
        self.device = device

    def infer(self, left, right, alpha):
        """Accept matching CHW/BCHW RGB float tensors in [0,1]."""
        import torch
        import torch.nn.functional as functional

        if not math.isfinite(alpha) or not 0 <= alpha <= 1:
            raise ValueError("Invalid interpolation timestep")
        unbatched = left.ndim == 3
        if unbatched:
            left, right = left.unsqueeze(0), right.unsqueeze(0)
        if left.shape != right.shape or left.ndim != 4 or left.shape[:2] != (1, 3):
            raise ValueError("GIMM requires one matching RGB frame pair")
        height, width = left.shape[-2:]
        if min(height, width) < 2:
            raise ValueError("GIMM frame dimensions are too small")
        left = left.to(device=self.device, dtype=torch.float32)
        right = right.to(device=self.device, dtype=torch.float32)
        if not bool(torch.isfinite(left).all() & torch.isfinite(right).all()):
            raise ValueError("Nonfinite GIMM input")
        if alpha == 0 or alpha == 1:
            output = left if alpha == 0 else right
            return output[0] if unbatched else output
        # Power-of-two flow downsampling follows the upstream 2K/4K approach;
        # divisibility is maintained after scaling, including odd source sizes.
        divisor = 1
        while max(height, width) / divisor > 1280:
            divisor *= 2
        ds = 1.0 / divisor
        padding_unit = 32 * divisor
        # Four RAFT correlation levels need at least a 2x2 coarsest grid.
        # 128 pixels before flow downsampling yields 16 -> 8 -> 4 -> 2.
        pad_h = max(128 * divisor, math.ceil(height / padding_unit) * padding_unit) - height
        pad_w = max(128 * divisor, math.ceil(width / padding_unit) * padding_unit) - width
        padding = (pad_w // 2, pad_w - pad_w // 2, pad_h // 2, pad_h - pad_h // 2)
        left = functional.pad(left, padding, mode="replicate")
        right = functional.pad(right, padding, mode="replicate")
        frames = torch.stack((left, right), dim=2)
        shape = frames.shape[-2:]
        with torch.inference_mode():
            coordinates = self.model.sample_coord_input(
                1, shape, [alpha], device=self.device, upsample_ratio=ds
            )
            timesteps = [torch.tensor([alpha], device=self.device, dtype=torch.float32)]
            output = self.model(frames, [(coordinates, None)], t=timesteps, ds_factor=ds)["imgt_pred"][0]
            output = output[..., padding[2]:padding[2] + height, padding[0]:padding[0] + width]
            if output.shape != (1, 3, height, width) or not bool(torch.isfinite(output).all()):
                raise RuntimeError("Invalid GIMM inference output")
            output = output.clamp(0, 1).contiguous()
        return output[0] if unbatched else output

    def selftest(self):
        import torch

        # Nonconstant, distinct endpoints and a non-midpoint force actual model
        # inference; an endpoint shortcut cannot satisfy the test.
        y, x = torch.meshgrid(torch.arange(128), torch.arange(128), indexing="ij")
        left = torch.stack((x / 127, y / 127, ((x // 16 + y // 16) % 2).float())).to(self.device)
        right = torch.roll(left, shifts=(3, 5), dims=(-2, -1))
        result = self.infer(left, right, 0.37)
        if result.shape != left.shape or result.std().item() < 0.01:
            raise RuntimeError("GIMM self-test produced trivial output")
        if torch.equal(result, left) or torch.equal(result, right):
            raise RuntimeError("GIMM self-test did not interpolate")
        return {"ok": True, "modelId": MODEL_ID, "sourceCommit": SOURCE_COMMIT, "device": str(self.device)}


def create(modeldir, sourcedir, device):
    import torch
    from safetensors.torch import load as load_safetensors

    # Hash bounded, inert safetensors bytes before instantiating reviewed code.
    states = {
        name: load_safetensors(_checked_bytes(modeldir, name, spec))
        for name, spec in MODEL_FILES.items()
    }
    model_class, raft_class = _load_source(sourcedir)
    model = model_class(torch.float32, _config())
    main_state = states["gimmvfi_r_arb_lpips_fp32.safetensors"]
    result = model.load_state_dict(main_state, strict=False)
    if result.missing_keys or any(not key.startswith("flow_estimator.") for key in result.unexpected_keys):
        raise RuntimeError("GIMM checkpoint does not match the reviewed architecture")
    raft = raft_class(_RaftArgs(small=False, mixed_precision=False, alternate_corr=False))
    raft.load_state_dict(states["raft-things_fp32.safetensors"], strict=True)
    model.flow_estimator = raft
    model = model.eval().to(device=device, dtype=torch.float32)
    model.requires_grad_(False)
    return Adapter(model, torch.device(device))
