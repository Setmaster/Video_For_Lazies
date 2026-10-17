// Persistent RGB8 RIFE worker. Wire contract: protocol.md.
#include "net.h"
#include "gpu.h"
#include "rife_ops.h"
#include <algorithm>
#include <array>
#include <cmath>
#include <csignal>
#include <cctype>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <iostream>
#include <limits>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>
#ifdef _WIN32
#include <fcntl.h>
#include <io.h>
#include <windows.h>
#endif

namespace {
struct Failure : std::runtime_error { using std::runtime_error::runtime_error; };
void require(bool condition, const char* diagnostic) { if (!condition) throw Failure(diagnostic); }
std::string json_string(const std::string& text) {
    std::string result = "\"";
    for (unsigned char c : text) {
        if (c == '"' || c == '\\') { result += '\\'; result += static_cast<char>(c); }
        else if (c < 32) { const char* h = "0123456789abcdef"; result += "\\u00"; result += h[c >> 4]; result += h[c & 15]; }
        else result += static_cast<char>(c);
    }
    return result + '"';
}
int positive_int(const std::string& value) {
    require(!value.empty() && value.size() <= 6, "invalid-dimensions");
    unsigned n = 0;
    for (char c : value) { require(c >= '0' && c <= '9', "invalid-dimensions"); n = n * 10 + static_cast<unsigned>(c - '0'); }
    require(n > 0 && n <= 8192, "invalid-dimensions");
    return static_cast<int>(n);
}
struct Options { enum Mode { Capabilities, SelfTest, Stream, AvailableSpace } mode = Capabilities; bool mode_set = false; std::filesystem::path model, space_path; int width = 0, height = 0; std::string gpu = "auto"; };
Options parse(const std::vector<std::string>& args) {
    Options o;
    bool model_set = false, gpu_set = false;
    for (size_t i = 1; i < args.size(); ++i) {
        const auto& a = args[i];
        if (a == "--available-space") {
            require(!o.mode_set && i + 1 < args.size(), "invalid-arguments");
            o.mode_set = true; o.mode = Options::AvailableSpace;
            o.space_path = std::filesystem::u8path(args[++i]);
        } else if (a == "--capabilities" || a == "--self-test" || a == "--stream") {
            require(!o.mode_set, "invalid-arguments"); o.mode_set = true;
            o.mode = a == "--stream" ? Options::Stream : a == "--self-test" ? Options::SelfTest : Options::Capabilities;
        } else {
            require(i + 1 < args.size(), "invalid-arguments");
            const auto& value = args[++i];
            if (a == "--model") { require(!model_set, "invalid-arguments"); model_set = true; o.model = std::filesystem::u8path(value); }
            else if (a == "--gpu") { require(!gpu_set, "invalid-arguments"); gpu_set = true; o.gpu = value; }
            else if (a == "--width") { require(o.width == 0, "invalid-arguments"); o.width = positive_int(value); }
            else if (a == "--height") { require(o.height == 0, "invalid-arguments"); o.height = positive_int(value); }
            else throw Failure("invalid-arguments");
        }
    }
    require(o.mode_set, "invalid-arguments");
    if (o.mode == Options::Capabilities || o.mode == Options::AvailableSpace) require(!model_set && !gpu_set && !o.width && !o.height, "invalid-arguments");
    else require(model_set && !o.model.empty(), "missing-model");
    if (o.mode == Options::Stream) {
        require(o.width && o.height, "invalid-dimensions");
        require(static_cast<uint64_t>(o.width) * o.height <= 3840ull * 2160ull, "frame-too-large");
    } else require(!o.width && !o.height, "invalid-arguments");
    return o;
}
struct GpuInstance {
    bool available;
    GpuInstance() : available(ncnn::create_gpu_instance() == 0) {}
    ~GpuInstance() { if (available) ncnn::destroy_gpu_instance(); }
};
bool hardware_device(int id) {
    const auto& info = ncnn::get_gpu_info(id);
    if (info.type() < 0 || info.type() == 3) return false;
    std::string name = std::string(info.device_name()) + " " + info.driver_name();
    std::transform(name.begin(), name.end(), name.begin(), [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    for (const auto* software : {"llvmpipe", "lavapipe", "swiftshader", "software"}) if (name.find(software) != std::string::npos) return false;
    return true;
}
int device_id(const std::string& choice, bool available) {
    if (choice == "-1") return -1;
    require(available && ncnn::get_gpu_count() > 0, "vulkan-unavailable");
    int id = -1;
    if (choice == "auto") {
        const int preferred = ncnn::get_default_gpu_index();
        if (preferred >= 0 && preferred < ncnn::get_gpu_count() && hardware_device(preferred)) id = preferred;
        else for (int candidate = 0; candidate < ncnn::get_gpu_count(); ++candidate) if (hardware_device(candidate)) { id = candidate; break; }
        require(id >= 0, "vulkan-hardware-unavailable");
    }
    else { require(choice.size() <= 3 && !choice.empty(), "invalid-device"); id = 0; for (char c : choice) { require(c >= '0' && c <= '9', "invalid-device"); id = id * 10 + c - '0'; } }
    require(id >= 0 && id < ncnn::get_gpu_count(), "invalid-device");
    require(ncnn::get_gpu_device(id) != nullptr, "vulkan-device-failed");
    return id;
}
std::string device_name(int id) { return id < 0 ? "CPU" : ncnn::get_gpu_info(id).device_name(); }
DEFINE_LAYER_CREATOR(Warp)
struct FileCloser { void operator()(FILE* f) const { if (f) fclose(f); } };
using File = std::unique_ptr<FILE, FileCloser>;
std::filesystem::path native_path(const std::filesystem::path& path) {
#ifdef _WIN32
    std::error_code ec;
    const auto absolute = std::filesystem::absolute(path, ec).lexically_normal();
    require(!ec, "invalid-path");
    const auto name = absolute.native();
    if (name.rfind(L"\\\\?\\", 0) == 0) return absolute;
    if (name.rfind(L"\\\\", 0) == 0) return std::filesystem::path(L"\\\\?\\UNC\\" + name.substr(2));
    return std::filesystem::path(L"\\\\?\\" + name);
#else
    return path;
#endif
}
File model_file(const std::filesystem::path& p, uintmax_t max_bytes) {
    const auto file_path = native_path(p);
    std::error_code ec;
    require(std::filesystem::is_regular_file(file_path, ec) && !ec, "model-file-unavailable");
    const auto bytes = std::filesystem::file_size(file_path, ec);
    require(!ec && bytes > 0 && bytes <= max_bytes, "invalid-model-size");
#ifdef _WIN32
    File f(_wfopen(file_path.c_str(), L"rb"));
#else
    File f(fopen(file_path.c_str(), "rb"));
#endif
    require(static_cast<bool>(f), "model-file-unavailable");
    return f;
}
class Engine {
    ncnn::Net net;
public:
    Engine(const std::filesystem::path& dir, int gpu) {
        net.opt.num_threads = 4;
        net.opt.use_vulkan_compute = gpu >= 0;
        net.opt.use_fp16_packed = gpu >= 0;
        net.opt.use_fp16_storage = gpu >= 0;
        net.opt.use_fp16_arithmetic = false;
        net.opt.use_int8_storage = false;
        if (gpu >= 0) net.set_vulkan_device(gpu);
        require(net.register_custom_layer("rife.Warp", Warp_layer_creator) == 0, "model-layer-registration-failed");
        auto param = model_file(dir / "flownet.param", 256 * 1024);
        auto weights = model_file(dir / "flownet.bin", 64 * 1024 * 1024);
        require(net.load_param(param.get()) == 0, "model-parameters-invalid");
        require(net.load_model(weights.get()) == 0, "model-weights-invalid");
    }
    std::vector<unsigned char> infer(const std::vector<unsigned char>& left, const std::vector<unsigned char>& right, int width, int height, float alpha) {
        require(std::isfinite(alpha) && alpha >= 0.f && alpha <= 1.f, "invalid-alpha");
        const size_t bytes = static_cast<size_t>(width) * height * 3;
        require(left.size() == bytes && right.size() == bytes, "invalid-frame-size");
        if (alpha == 0.f) return left;
        if (alpha == 1.f) return right;
        const int pw = (width + 63) / 64 * 64, ph = (height + 63) / 64 * 64;
        ncnn::Mat a(pw, ph, 3), b(pw, ph, 3), t(pw, ph, 1);
        require(!a.empty() && !b.empty() && !t.empty(), "allocation-failed");
        a.fill(0.f); b.fill(0.f); t.fill(alpha);
        for (int c = 0; c < 3; ++c) {
            ncnn::Mat ac = a.channel(c), bc = b.channel(c);
            for (int y = 0; y < height; ++y) {
                float* ap = ac.row(y); float* bp = bc.row(y);
                for (int x = 0; x < width; ++x) {
                    const size_t ix = (static_cast<size_t>(y) * width + x) * 3 + c;
                    ap[x] = left[ix] / 255.f; bp[x] = right[ix] / 255.f;
                }
            }
        }
        auto ex = net.create_extractor();
        require(ex.input("in0", a) == 0 && ex.input("in1", b) == 0 && ex.input("in2", t) == 0, "model-input-failed");
        ncnn::Mat out;
        require(ex.extract("out0", out) == 0, "inference-failed");
        require(!out.empty() && out.w == pw && out.h == ph && out.c == 3 && out.elempack == 1 && out.elemsize == sizeof(float), "invalid-output-shape");
        std::vector<unsigned char> result(bytes);
        for (int c = 0; c < 3; ++c) {
            const ncnn::Mat channel = out.channel(c);
            for (int y = 0; y < height; ++y) {
                const float* row = channel.row(y);
                for (int x = 0; x < width; ++x) {
                    require(std::isfinite(row[x]), "nonfinite-output");
                    result[(static_cast<size_t>(y) * width + x) * 3 + c] = static_cast<unsigned char>(std::round(std::clamp(row[x], 0.f, 1.f) * 255.f));
                }
            }
        }
        return result;
    }
};
void self_test(Engine& engine, int gpu) {
    const int w = 65, h = 37;
    std::vector<unsigned char> a(w * h * 3), b(a.size());
    for (int y = 0; y < h; ++y) for (int x = 0; x < w; ++x) {
        size_t i = (y * w + x) * 3;
        a[i] = 180; a[i + 1] = static_cast<unsigned char>(y * 3); a[i + 2] = 25;
        b[i] = 25; b[i + 1] = static_cast<unsigned char>(y * 3); b[i + 2] = 180;
    }
    const auto p = engine.infer(a, b, w, h, 0.25f), q = engine.infer(a, b, w, h, 0.75f);
    uint64_t change = 0, left_error = 0, right_error = 0, total = 0;
    for (size_t i = 0; i < a.size(); ++i) {
        change += std::abs(int(p[i]) - int(q[i])); left_error += std::abs(int(p[i]) - int(a[i])); right_error += std::abs(int(p[i]) - int(b[i])); total += p[i];
    }
    require(change > a.size() && left_error > a.size() && right_error > a.size() && total > a.size() * 8 && total < a.size() * 247, "self-test-failed");
    std::cout << "{\"protocol\":1,\"ok\":true,\"deviceName\":" << json_string(device_name(gpu)) << "}\n";
}
bool read_alpha(float& alpha) {
    std::array<unsigned char, 4> raw{};
    std::cin.read(reinterpret_cast<char*>(raw.data()), raw.size());
    const auto count = std::cin.gcount();
    if (count == 0 && std::cin.eof()) return false;
    require(count == 4, "truncated-record");
    const uint32_t bits = uint32_t(raw[0]) | (uint32_t(raw[1]) << 8) | (uint32_t(raw[2]) << 16) | (uint32_t(raw[3]) << 24);
    static_assert(sizeof(float) == 4 && std::numeric_limits<float>::is_iec559, "IEEE float32 required");
    std::memcpy(&alpha, &bits, 4);
    require(std::isfinite(alpha) && alpha >= 0.f && alpha <= 1.f, "invalid-alpha");
    return true;
}
int run(const std::vector<std::string>& args) {
    const Options o = parse(args);
    if (o.mode == Options::AvailableSpace) {
        std::error_code ec;
        const auto space = std::filesystem::space(native_path(o.space_path), ec);
        require(!ec && space.available != static_cast<uintmax_t>(-1), "space-query-failed");
        std::cout << "{\"availableBytes\":" << space.available << "}\n";
        return 0;
    }
    GpuInstance instance;
    if (o.mode == Options::Capabilities) {
        std::cout << "{\"protocol\":1,\"devices\":[";
        bool first = true;
        if (instance.available) for (int i = 0; i < ncnn::get_gpu_count(); ++i) {
            if (!hardware_device(i)) continue;
            if (!first) std::cout << ',';
            first = false;
            std::cout << "{\"id\":" << i << ",\"name\":" << json_string(device_name(i)) << '}';
        }
        std::cout << "],\"cpu\":true}\n"; return 0;
    }
    const int gpu = device_id(o.gpu, instance.available);
    Engine engine(o.model, gpu);
    if (o.mode == Options::SelfTest) { self_test(engine, gpu); return 0; }
    const size_t bytes = static_cast<size_t>(o.width) * o.height * 3;
    std::vector<unsigned char> a(bytes), b(bytes);
    float alpha;
    while (read_alpha(alpha)) {
        std::cin.read(reinterpret_cast<char*>(a.data()), static_cast<std::streamsize>(bytes));
        require(std::cin.gcount() == static_cast<std::streamsize>(bytes), "truncated-record");
        std::cin.read(reinterpret_cast<char*>(b.data()), static_cast<std::streamsize>(bytes));
        require(std::cin.gcount() == static_cast<std::streamsize>(bytes), "truncated-record");
        const auto output = engine.infer(a, b, o.width, o.height, alpha);
        std::cout.write(reinterpret_cast<const char*>(output.data()), static_cast<std::streamsize>(output.size()));
        std::cout.flush(); require(std::cout.good(), "output-write-failed");
    }
    return 0;
}
int entry(const std::vector<std::string>& args) {
    try { return run(args); }
    catch (const Failure& e) { std::cerr << "vfl-rife: " << e.what() << '\n'; }
    catch (const std::bad_alloc&) { std::cerr << "vfl-rife: allocation-failed\n"; }
    catch (...) { std::cerr << "vfl-rife: native-failure\n"; }
    return 2;
}
}
#ifdef _WIN32
int wmain(int argc, wchar_t** argv) {
    _setmode(_fileno(stdin), _O_BINARY); _setmode(_fileno(stdout), _O_BINARY);
    std::vector<std::string> args;
    for (int i = 0; i < argc; ++i) {
        const int size = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, argv[i], -1, nullptr, 0, nullptr, nullptr);
        if (size <= 0) { std::cerr << "vfl-rife: invalid-arguments\n"; return 2; }
        std::string value(static_cast<size_t>(size), '\0');
        WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, argv[i], -1, value.data(), size, nullptr, nullptr);
        value.pop_back(); args.push_back(std::move(value));
    }
    return entry(args);
}
#else
int main(int argc, char** argv) {
    std::signal(SIGPIPE, SIG_IGN);
    return entry(std::vector<std::string>(argv, argv + argc));
}
#endif
