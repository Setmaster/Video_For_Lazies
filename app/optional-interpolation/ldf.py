"""LDF-VFI sequence adapter for the optional interpolation worker.

Sampling follows xypeng9903/LDF-VFI, commit
61b34d2379df8a313e8e4cb467cc2f74c52b45d7 (Apache-2.0).
The component installer verifies the exact source/model manifest before create().
This module never downloads files or owns presentation timestamps or audio.
"""

from __future__ import annotations

import importlib
from pathlib import Path
import sys
import types


_WINDOW = 60
_TILE = 20
_STEPS = 16
_SOURCE_COMMIT = "61b34d2379df8a313e8e4cb467cc2f74c52b45d7"


def _sampling_windows(count, factor):
    """Yield bounded sampler work in compute order, including skip lookahead."""
    if type(count) is not int or count < 1:
        raise ValueError("LDF requires at least one input frame.")
    if type(factor) is not int or not 2 <= factor <= 16:
        raise ValueError("LDF requires an integer multiplier from 2 to 16.")
    yield "first", 0, 0, 40
    position = 40
    while position + _TILE < count * factor:
        yield "skip", position, position + _TILE, _TILE
        yield "concat", position - _TILE, position, _TILE
        position += 2 * _TILE
    if position < count * factor:
        yield "tail", position - _TILE, position, 40


def _source_modules(source_dir):
    """Import only modules within the installer's verified source namespaces."""
    root = Path(source_dir).resolve(strict=True)
    for name, relative in (
        ("training", "training"),
        ("training.models", "training/models"),
        ("training.distributed", "training/distributed"),
    ):
        directory = root / relative
        if not directory.is_dir() or directory.resolve() != directory:
            raise RuntimeError("LDF source package is missing or redirected.")
        existing = sys.modules.get(name)
        if existing is not None:
            if getattr(existing, "__vfl_source_root__", None) != str(root):
                raise RuntimeError("LDF source namespace is already in use.")
            continue
        # Deliberately do not execute upstream package __init__.py files.
        module = types.ModuleType(name)
        module.__path__ = [str(directory)]
        module.__package__ = name
        module.__vfl_source_root__ = str(root)
        sys.modules[name] = module
    precond = importlib.import_module("training.models.precond")
    return precond



def _verified_transformer(result):
    # diffusers 0.33.1 returns (model, loading_info) when explicitly requested.
    fields = {"missing_keys", "unexpected_keys", "mismatched_keys", "error_msgs"}
    if not isinstance(result, tuple) or len(result) != 2:
        raise RuntimeError("LDF transformer loader returned no verification details.")
    transformer, info = result
    if not isinstance(info, dict) or set(info) != fields:
        raise RuntimeError("LDF transformer loader returned invalid verification details.")
    if any(not isinstance(info[name], (list, tuple)) or info[name] for name in fields):
        raise RuntimeError("LDF transformer weights do not match the expected model.")
    return transformer


def create(model_dir, source_dir, device):
    """Create a local-only CUDA/BF16 model after manifest verification."""
    import torch

    device = torch.device(device)
    if device.type != "cuda" or not torch.cuda.is_available():
        raise RuntimeError("LDF requires a supported NVIDIA CUDA GPU.")
    with torch.cuda.device(device):
        if not torch.cuda.is_bf16_supported():
            raise RuntimeError("LDF requires GPU support for bfloat16.")
    root = Path(model_dir).resolve(strict=True)
    for relative in (
        "transformer/config.json",
        "transformer/diffusion_pytorch_model.safetensors",
        "Wan2.1_VAE_cond_v2.pth",
    ):
        path = root / relative
        if not path.is_file() or path.resolve() != path:
            raise RuntimeError("An LDF model component is missing or redirected.")
    modules = _source_modules(source_dir)
    state = torch.load(
        root / "Wan2.1_VAE_cond_v2.pth", map_location="cpu", weights_only=True
    )
    if not isinstance(state, dict) or not state:
        raise RuntimeError("The LDF VAE checkpoint is not a tensor state dictionary.")
    if not all(isinstance(key, str) and torch.is_tensor(value) for key, value in state.items()):
        raise RuntimeError("The LDF VAE checkpoint contains unsupported state.")

    class StrictVae(modules.Wan2_1SpatialTiledConditionEncoder3Dv2):
        def init(self, selected_device):
            if self.vae is not None:
                return
            self.vae = modules.WanVAECondition_v2(
                dim=96,
                z_dim=16,
                dim_mult=[1, 2, 4, 4],
                num_res_blocks=2,
                attn_scales=[],
                temperal_downsample=[False, True, True],
                dropout=0.0,
            )
            # Unlike upstream's strict=False, missing/extra/wrong-shaped tensors fail.
            self.vae.load_state_dict(state, strict=True, assign=True)
            self.vae.to(device=selected_device, dtype=self.dtype).requires_grad_(False).eval()
            self.mean = torch.tensor(
                [-0.7571, -0.7089, -0.9113, 0.1075, -0.1745, 0.9653, -0.1517, 1.5508,
                 0.4134, -0.0715, 0.5517, -0.3632, -0.1922, -0.9497, 0.2503, -0.2921],
                dtype=self.dtype, device=selected_device,
            )
            self.std = torch.tensor(
                [2.8184, 1.4541, 2.3275, 2.6558, 1.2196, 1.7708, 2.6052, 2.0743,
                 3.2687, 2.1526, 2.8652, 1.5579, 1.6382, 1.1253, 2.8251, 1.9160],
                dtype=self.dtype, device=selected_device,
            )
            self.scale = [self.mean, 1.0 / self.std]

    tiled = dict(
        tile_sample_min_height=256, tile_sample_min_width=256,
        tile_sample_min_time=_TILE, tile_sample_stride_height=192,
        tile_sample_stride_width=192, spatial_compression_ratio=8,
        temporal_compression_ratio=4,
    )
    vae = StrictVae(str(root / "Wan2.1_VAE_cond_v2.pth"), 16, **tiled)
    encoder = StrictVae(str(root / "Wan2.1_VAE_cond_v2.pth"), 16, **tiled)
    vae.init(device)
    encoder.init(device)
    # Both wrappers are initialized, so their lazy init never needs the CPU state again.
    state.clear()
    transformer = _verified_transformer(modules.WanTransformer3DModel.from_pretrained(
        str(root), subfolder="transformer", local_files_only=True,
        use_safetensors=True, torch_dtype=torch.bfloat16, output_loading_info=True,
    ))
    transformer.set_attention_type("slide_chunk_all_block_2x1x1")
    transformer.to(device=device, dtype=torch.bfloat16).requires_grad_(False).eval()
    model = modules.Precond(
        transformer=transformer, vae=vae, lq_encoder=encoder,
        msk_encoder=modules.MaskSpatialTiledEncoder3D(**tiled),
    )
    model.to(device=device, dtype=torch.bfloat16).eval()
    return Adapter(model, device)


class Adapter:
    """Persistent model with bounded 60-frame diffusion windows and AR context."""

    def __init__(self, model, device):
        self.model = model
        self.device = device

    def _conditioning(self, frames, factor, start, height, width):
        import torch

        first = (start + factor - 1) // factor
        stop = min(len(frames), (start + _WINDOW + factor - 1) // factor)
        if first >= stop:
            raise RuntimeError("An LDF sampling window has no source anchors.")
        anchors = []
        for index in range(first, stop):
            frame = frames[index]
            if (not torch.is_tensor(frame) or tuple(frame.shape) != (1, 3, height, width)
                    or frame.dtype != torch.float32):
                raise ValueError("LDF input pictures must be float32 RGB tensors with matching dimensions.")
            if not torch.isfinite(frame).all() or frame.min() < 0 or frame.max() > 1:
                raise ValueError("LDF input RGB values must be finite and between zero and one.")
            anchors.append(frame.to(device=self.device))
        positions = [index * factor - start for index in range(first, stop)]
        # Ties select the earlier anchor, as in upstream distances.argmin().
        nearest = [min(range(len(positions)), key=lambda index: abs(t - positions[index]))
                   for t in range(_WINDOW)]
        source = torch.cat(anchors, dim=0)
        source = source.index_select(0, torch.tensor(nearest, device=self.device))
        source = (source.permute(1, 0, 2, 3).unsqueeze(0).mul(255).round()
                  .to(torch.bfloat16).div(127.5).sub(1))
        mask = torch.zeros(_WINDOW, device=self.device, dtype=torch.bfloat16)
        mask[positions] = 1
        mask = mask.reshape(1, 1, _WINDOW, 1, 1).expand(1, 1, _WINDOW, height, width)
        return source, mask

    def _decode(self, latent, source, mask, height, width):
        import torch
        import torch.nn.functional as functional
        from einops import rearrange, repeat

        latent = rearrange(latent, "1 nt nh nw c t h w -> 1 nt c t (nh h) (nw w)")
        decode_h, decode_w = latent.shape[-2] * 8, latent.shape[-1] * 8
        source = functional.pad(source, (0, decode_w - width, 0, decode_h - height))
        source = rearrange(source, "b c (nt t) h w -> b nt c t h w", nt=latent.shape[1])
        mask = repeat(mask[..., 0, 0], "b c t -> b c t h w", h=decode_h, w=decode_w)
        mask = rearrange(mask, "b c (nt t) h w -> b nt c t h w", nt=latent.shape[1])
        prediction = self.model.vae.decode(latent, source, mask)[..., :height, :width]
        if tuple(prediction.shape) != (1, 3, latent.shape[1] * _TILE, height, width):
            raise RuntimeError("LDF returned an invalid decoded picture shape.")
        if not torch.isfinite(prediction).all():
            raise RuntimeError("LDF produced non-finite pixels.")
        # Preserve upstream's RGB8 boundary, then expose the common float32 protocol.
        return (prediction.add(1).mul(127.5).clamp(0, 255).byte()
                .permute(2, 0, 1, 3, 4).to(device="cpu", dtype=torch.float32).div_(255))

    def infer_sequence(self, frames, factor):
        """Yield exactly (N-1)*factor+1 RGB tensors, without whole-clip buffering."""
        import torch

        count = len(frames)
        plan = _sampling_windows(count, factor)
        # Validate the plan before indexing the input.
        first_action = next(plan)
        first = frames[0]
        if (not torch.is_tensor(first) or first.ndim != 4 or tuple(first.shape[:2]) != (1, 3)
                or first.dtype != torch.float32 or min(first.shape[-2:]) < 1):
            raise ValueError("LDF input pictures must have shape [1,3,H,W] and dtype float32.")
        height, width = first.shape[-2:]
        if count == 1:
            if not torch.isfinite(first).all() or first.min() < 0 or first.max() > 1:
                raise ValueError("LDF input RGB values must be finite and between zero and one.")
            yield first.detach().to(device="cpu", dtype=torch.float32).clone()
            return
        expected = (count - 1) * factor + 1
        emitted = 0
        previous = following = next_previous = skipped = None
        # Fork the process RNG so retries/scenes have a reproducible private sampling stream.
        with torch.random.fork_rng(devices=[self.device]), torch.inference_mode():
            torch.manual_seed(42)
            times = torch.linspace(1.0, 0.0, steps=_STEPS + 1).numpy()
            times = 8 * times / (1 + 7 * times)
            from itertools import chain
            for kind, start, output_start, output_count in chain([first_action], plan):
                source, mask = self._conditioning(frames, factor, start, height, width)
                condition = self.model.lq_encoder.encode(source, for_train=True)
                encoded_mask = self.model.msk_encoder.encode(mask, for_train=True)
                if kind in ("first", "skip"):
                    latent = torch.randn_like(condition)
                    for step in range(_STEPS):
                        time = torch.tensor([times[step]], device=self.device, dtype=torch.bfloat16)
                        time = time.expand(latent.shape[:-4]).contiguous()
                        velocity = self.model.predict_v(latent, time, condition, encoded_mask)
                        latent = latent + velocity * (times[step + 1] - times[step])
                    if kind == "first":
                        latent = latent[:, :2]
                        previous = latent[:, -1:].clone()
                        prediction = self._decode(latent, source[:, :, :40], mask[:, :, :40], height, width)
                    else:
                        latent = latent[:, 1:2]
                        following = latent.clone()
                        next_previous = latent.clone()
                        skipped = self._decode(latent, source[:, :, 20:40], mask[:, :, 20:40], height, width)
                        continue
                else:
                    if kind == "concat":
                        middle = torch.randn_like(condition[:, 1:2])
                        left = previous * 0.9 + torch.randn_like(previous) * 0.1
                    else:
                        left = previous * 0.9 + torch.randn_like(previous) * 0.1
                        middle = torch.randn_like(condition[:, 1:])
                    right = (following * 0.9 + torch.randn_like(following) * 0.1
                             if kind == "concat" else None)
                    for step in range(_STEPS):
                        pieces = [left, middle] + ([right] if right is not None else [])
                        latent = torch.cat(pieces, dim=1)
                        middle_time = torch.full(middle.shape[:-4], times[step], device=self.device, dtype=torch.bfloat16)
                        left_time = torch.full(left.shape[:-4], 0.1, device=self.device, dtype=torch.bfloat16)
                        time = torch.cat([left_time, middle_time] + ([left_time] if right is not None else []), dim=1)
                        velocity = self.model.predict_v(latent, time, condition, encoded_mask)[:, 1:]
                        if right is not None:
                            velocity = velocity[:, :1]
                        middle = middle + velocity * (times[step + 1] - times[step])
                    end = 40 if kind == "concat" else 60
                    prediction = self._decode(middle, source[:, :, 20:end], mask[:, :, 20:end], height, width)
                    if kind == "concat":
                        previous = next_previous
                if emitted != output_start or len(prediction) != output_count:
                    raise RuntimeError("LDF returned an invalid sampling section.")
                chunks = [prediction] + ([skipped] if kind == "concat" else [])
                for chunk in chunks:
                    for picture in chunk:
                        if emitted >= expected:
                            return
                        yield picture.contiguous()
                        emitted += 1
                if kind == "concat":
                    skipped = following = next_previous = None
                if emitted >= expected:
                    return
        if emitted != expected:
            raise RuntimeError("LDF returned an incomplete frame sequence.")
