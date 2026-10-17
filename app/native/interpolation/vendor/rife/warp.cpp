// rife implemented with ncnn library

#include "rife_ops.h"
#include <algorithm>
#include <cmath>

#include "warp.comp.hex.h"
#include "warp_pack4.comp.hex.h"

using namespace ncnn;

Warp::Warp()
{
    support_vulkan = true;

    pipeline_warp = 0;
    pipeline_warp_pack4 = 0;
}

int Warp::create_pipeline(const Option& opt)
{
    if (!vkdev)
        return 0;

    std::vector<vk_specialization_type> specializations(0 + 0);

    // pack1
    {
        static std::vector<uint32_t> spirv;
        static ncnn::Mutex lock;
        {
            ncnn::MutexLockGuard guard(lock);
            if (spirv.empty())
            {
                if (compile_spirv_module(warp_comp_data, sizeof(warp_comp_data), opt, spirv) != 0 || spirv.empty()) return -1;
            }
        }

        pipeline_warp = new Pipeline(vkdev);
        pipeline_warp->set_optimal_local_size_xyz();
        if (pipeline_warp->create(spirv.data(), spirv.size() * 4, specializations) != 0) return -1;
    }

    // pack4
    {
        static std::vector<uint32_t> spirv;
        static ncnn::Mutex lock;
        {
            ncnn::MutexLockGuard guard(lock);
            if (spirv.empty())
            {
                if (compile_spirv_module(warp_pack4_comp_data, sizeof(warp_pack4_comp_data), opt, spirv) != 0 || spirv.empty()) return -1;
            }
        }

        pipeline_warp_pack4 = new Pipeline(vkdev);
        pipeline_warp_pack4->set_optimal_local_size_xyz();
        if (pipeline_warp_pack4->create(spirv.data(), spirv.size() * 4, specializations) != 0) return -1;
    }

    return 0;
}

int Warp::destroy_pipeline(const Option&)
{
    delete pipeline_warp;
    pipeline_warp = 0;

    delete pipeline_warp_pack4;
    pipeline_warp_pack4 = 0;

    return 0;
}

int Warp::forward(const std::vector<Mat>& bottom_blobs, std::vector<Mat>& top_blobs, const Option&) const
{
    if (bottom_blobs.size() != 2 || top_blobs.size() != 1) return -1;
    const Mat& image_blob = bottom_blobs[0];
    const Mat& flow_blob = bottom_blobs[1];

    int w = image_blob.w;
    int h = image_blob.h;
    int channels = image_blob.c;

    if (image_blob.empty() || flow_blob.empty() || w <= 0 || h <= 0 || flow_blob.w != w || flow_blob.h != h || flow_blob.c != 2 || image_blob.elempack != 1 || flow_blob.elempack != 1 || image_blob.elemsize != sizeof(float) || flow_blob.elemsize != sizeof(float)) return -1;

    Mat& top_blob = top_blobs[0];
    top_blob.create(w, h, channels);
    if (top_blob.empty())
        return -100;

    for (int q = 0; q < channels; q++)
    {
        float* outptr = top_blob.channel(q);

        const Mat image = image_blob.channel(q);

        const float* fxptr = flow_blob.channel(0);
        const float* fyptr = flow_blob.channel(1);

        for (int y = 0; y < h; y++)
        {
            for (int x = 0; x < w; x++)
            {
                float flow_x = fxptr[0];
                float flow_y = fyptr[0];

                // Clamp before floor/conversion, matching border grid sampling and
                // avoiding undefined float-to-int conversion for large displacements.
                float sample_x = std::clamp(x + flow_x, 0.f, float(w - 1));
                float sample_y = std::clamp(y + flow_y, 0.f, float(h - 1));
                if (!std::isfinite(sample_x) || !std::isfinite(sample_y)) return -1;

                // bilinear interpolate
                float v;
                {
                    int x0 = static_cast<int>(floor(sample_x));
                    int y0 = static_cast<int>(floor(sample_y));
                    int x1 = x0 + 1;
                    int y1 = y0 + 1;

                    x0 = std::min(std::max(x0, 0), w - 1);
                    y0 = std::min(std::max(y0, 0), h - 1);
                    x1 = std::min(std::max(x1, 0), w - 1);
                    y1 = std::min(std::max(y1, 0), h - 1);

                    float alpha = sample_x - x0;
                    float beta = sample_y - y0;

                    float v0 = image.row(y0)[x0];
                    float v1 = image.row(y0)[x1];
                    float v2 = image.row(y1)[x0];
                    float v3 = image.row(y1)[x1];

                    float v4 = v0 * (1 - alpha) + v1 * alpha;
                    float v5 = v2 * (1 - alpha) + v3 * alpha;

                    v = v4 * (1 - beta) + v5 * beta;
                }

                outptr[0] = v;

                outptr += 1;

                fxptr += 1;
                fyptr += 1;
            }
        }
    }

    return 0;
}

int Warp::forward(const std::vector<VkMat>& bottom_blobs, std::vector<VkMat>& top_blobs, VkCompute& cmd, const Option& opt) const
{
    if (bottom_blobs.size() != 2 || top_blobs.size() != 1) return -1;
    const VkMat& image_blob = bottom_blobs[0];
    const VkMat& flow_blob = bottom_blobs[1];

    int w = image_blob.w;
    int h = image_blob.h;
    int channels = image_blob.c;
    size_t elemsize = image_blob.elemsize;
    int elempack = image_blob.elempack;

    if (image_blob.empty() || flow_blob.empty() || w <= 0 || h <= 0 || flow_blob.w != w || flow_blob.h != h || flow_blob.c != 2 || flow_blob.elempack != 1 || (elempack != 1 && elempack != 4)) return -1;

    VkMat& top_blob = top_blobs[0];
    top_blob.create(w, h, channels, elemsize, elempack, opt.blob_vkallocator);
    if (top_blob.empty())
        return -100;

    std::vector<VkMat> bindings(3);
    bindings[0] = image_blob;
    bindings[1] = flow_blob;
    bindings[2] = top_blob;

    std::vector<vk_constant_type> constants(4);
    constants[0].i = top_blob.w;
    constants[1].i = top_blob.h;
    constants[2].i = top_blob.c;
    constants[3].i = static_cast<int>(top_blob.cstep);

    if (elempack == 4)
    {
        if (!pipeline_warp_pack4) return -1;
        cmd.record_pipeline(pipeline_warp_pack4, bindings, constants, top_blob);
    }
    else // if (elempack == 1)
    {
        if (!pipeline_warp) return -1;
        cmd.record_pipeline(pipeline_warp, bindings, constants, top_blob);
    }

    return 0;
}
