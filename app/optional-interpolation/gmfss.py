"""GMFSS Fortuna Union adapter for VFL's verified, private Torch runtime.

The enclosing worker owns byte verification, process isolation, frame transport,
the source clock, and endpoint frames. This adapter imports only the reviewed
source allowlist, uses eager Torch splatting, and never imports upstream CLIs or
VapourSynth/CuPy package entry points.
"""

from __future__ import annotations

import hashlib
import importlib
import math
from pathlib import Path
import sys
import types


def _modules(source_dir: Path):
    root = (source_dir / "gmfss_model").resolve(strict=True)
    if not root.is_dir():
        raise ValueError("GMFSS source directory is unavailable.")
    name = "_vfl_gmfss_" + hashlib.sha256(str(root).encode()).hexdigest()[:16]
    for package_name, directory in ((name, root), (name + ".gmflow", root / "gmflow")):
        if package_name not in sys.modules:
            # Never execute upstream package initializers.
            package = types.ModuleType(package_name)
            package.__path__ = [str(directory)]
            package.__package__ = package_name
            sys.modules[package_name] = package
    return {
        module: importlib.import_module(f"{name}.{module}")
        for module in (
            "FeatureNet", "MetricNet", "FusionNet_u", "IFNet_HDv3",
            "warplayer", "gmflow.gmflow", "softsplat_torch",
        )
    }


def _load_state(torch, network, path: Path):
    if not path.is_file() or path.is_symlink() or not 0 < path.stat().st_size <= 64 * 1024 * 1024:
        raise ValueError("GMFSS checkpoint is unavailable or exceeds its limit.")
    state = torch.load(path, map_location="cpu", weights_only=True)
    expected = network.state_dict()
    if not isinstance(state, dict) or state.keys() != expected.keys():
        raise ValueError("GMFSS checkpoint has unexpected parameter names.")
    for key, value in state.items():
        reference = expected[key]
        if (
            not isinstance(key, str)
            or not isinstance(value, torch.Tensor)
            or value.shape != reference.shape
            or value.dtype != reference.dtype
        ):
            raise ValueError("GMFSS checkpoint has unexpected parameter tensors.")
    network.load_state_dict(state, strict=True)
    return network


class _Union:
    def __init__(self, torch, model_dir: Path, source_dir: Path, device: str):
        self.torch = torch
        self.functional = torch.nn.functional
        modules = _modules(source_dir)
        self.warp = modules["softsplat_torch"].softsplat
        self.features = modules["FeatureNet"].FeatureNet()
        self.flow = modules["gmflow.gmflow"].GMFlow()
        self.metrics = modules["MetricNet"].MetricNet()
        self.fusion = modules["FusionNet_u"].GridNet()
        self.rife = modules["IFNet_HDv3"].IFNet(ensemble=False)
        networks = (
            (self.features, "feat_union.pkl"),
            (self.flow, "flownet.pkl"),
            (self.metrics, "metric_union.pkl"),
            (self.fusion, "fusionnet_union.pkl"),
            (self.rife, "rife.pkl"),
        )
        for network, filename in networks:
            _load_state(torch, network, model_dir / filename)
            network.eval().requires_grad_(False).to(device)
        self.device = next(self.features.parameters()).device
        # Scope upstream shape/device caches to this model process and device.
        modules["MetricNet"].backwarp_tenGrid.clear()
        modules["warplayer"].backwarp_tenGrid.clear()
        modules["softsplat_torch"].grid_cache.clear()
        modules["softsplat_torch"].batch_cache.clear()
        self.modules = modules
        self.last_shape = None

    def _resize(self, tensor, scale):
        return self.functional.interpolate(
            tensor, scale_factor=scale, mode="bilinear", align_corners=False,
        )

    def infer(self, left, right, alpha: float):
        torch = self.torch
        if (
            not isinstance(left, torch.Tensor)
            or not isinstance(right, torch.Tensor)
            or left.ndim != 4 or left.shape[:2] != (1, 3)
            or right.shape != left.shape
            or left.dtype != torch.float32 or right.dtype != torch.float32
            or left.device != self.device or right.device != self.device
            or not math.isfinite(alpha) or not 0 < alpha < 1
        ):
            raise ValueError("GMFSS expects matching RGB float32 pictures and an interior timestep.")
        height, width = left.shape[-2:]
        if height < 1 or width < 1:
            raise ValueError("GMFSS picture dimensions must be positive.")
        padded_height = max(64, (height + 63) // 64 * 64)
        padded_width = max(64, (width + 63) // 64 * 64)
        shape = (padded_height, padded_width)
        if shape != self.last_shape:
            self.modules["MetricNet"].backwarp_tenGrid.clear()
            self.modules["warplayer"].backwarp_tenGrid.clear()
            self.modules["softsplat_torch"].grid_cache.clear()
            self.modules["softsplat_torch"].batch_cache.clear()
            self.last_shape = shape
        with torch.inference_mode():
            padding = (0, padded_width - width, 0, padded_height - height)
            img0 = self.functional.pad(left, padding)
            img1 = self.functional.pad(right, padding)
            feat0 = self.features(img0)
            feat1 = self.features(img1)
            half0, half1 = self._resize(img0, 0.5), self._resize(img1, 0.5)
            flow01, flow10 = self.flow(half0, half1), self.flow(half1, half0)
            metric0, metric1 = self.metrics(half0, half1, flow01, flow10)
            forward, backward = flow01 * alpha, flow10 * (1 - alpha)
            weight0, weight1 = metric0 * alpha, metric1 * (1 - alpha)
            splat0 = self.warp(half0, forward, weight0, "soft")
            splat1 = self.warp(half1, backward, weight1, "soft")
            # HolyWu IFNet expands a scalar tensor with repeat(), unlike the
            # original Fortuna wrapper which also accepts a Python float.
            timestep = torch.full(
                (1, 1, 1, 1), alpha, dtype=half0.dtype, device=half0.device,
            )
            rife = self.rife(half0, half1, timestep)
            fused_features = []
            for level, scale in enumerate((1.0, 0.5, 0.25)):
                if scale == 1:
                    flow0, flow1 = forward, backward
                    mask0, mask1 = weight0, weight1
                else:
                    flow0 = self._resize(forward, scale) * scale
                    flow1 = self._resize(backward, scale) * scale
                    mask0 = self._resize(weight0, scale)
                    mask1 = self._resize(weight1, scale)
                fused_features.append(torch.cat((
                    self.warp(feat0[level], flow0, mask0, "soft"),
                    self.warp(feat1[level], flow1, mask1, "soft"),
                ), dim=1))
            result = self.fusion(
                torch.cat((splat0, rife, splat1), dim=1), *fused_features,
            )
            if result.shape != img0.shape or not torch.isfinite(result).all():
                raise ValueError("GMFSS produced an invalid picture.")
            return result[:, :, :height, :width].clamp(0, 1).contiguous()


def create(model_dir: Path, source_dir: Path, device: str):
    """Construct the Union model after the enclosing worker verifies its files."""
    import torch

    return _Union(torch, Path(model_dir), Path(source_dir), device)
