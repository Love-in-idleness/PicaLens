#include "gpu.h"
#include "picalens-shaders.h"

#include <algorithm>
#include <array>
#include <cmath>
#include <drm_fourcc.h>
#include <hyprland/src/render/OpenGL.hpp>
#include <hyprland/src/render/Renderer.hpp>
#include <utility>

namespace PicaLens {
namespace {

constexpr auto vertex = R"(#version 310 es
void main() {
    vec2 corner = vec2((gl_VertexID << 1) & 2, gl_VertexID & 2);
    gl_Position = vec4(corner * 2.0 - 1.0, 0.0, 1.0);
}
)";
constexpr auto fragment = R"(#version 310 es
precision highp float;
precision highp int;
uniform sampler2D inputImage;
uniform vec2 outputSize;
out vec4 outputColor;
)";

class DrawState {
  public:
    DrawState() {
        glGetIntegerv(GL_READ_FRAMEBUFFER_BINDING, &m_read);
        glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &m_draw);
        glGetIntegerv(GL_VIEWPORT, m_viewport.data());
        glGetIntegerv(GL_VERTEX_ARRAY_BINDING, &m_vao);
        glGetIntegerv(GL_CURRENT_PROGRAM, &m_program);
        glGetIntegerv(GL_ACTIVE_TEXTURE, &m_unit);
        glActiveTexture(GL_TEXTURE0);
        glGetIntegerv(GL_TEXTURE_BINDING_2D, &m_texture);
        glGetIntegerv(GL_SAMPLER_BINDING, &m_sampler);
        glGetFloatv(GL_COLOR_CLEAR_VALUE, m_clear.data());
        glBindSampler(0, 0);
        glDisable(GL_SCISSOR_TEST);
        glDisable(GL_BLEND);
    }

    ~DrawState() {
        glBindFramebuffer(GL_READ_FRAMEBUFFER, m_read);
        glBindFramebuffer(GL_DRAW_FRAMEBUFFER, m_draw);
        glViewport(m_viewport[0], m_viewport[1], m_viewport[2], m_viewport[3]);
        glBindVertexArray(m_vao);
        glUseProgram(m_program);
        glBindTexture(GL_TEXTURE_2D, m_texture);
        glBindSampler(0, m_sampler);
        glActiveTexture(m_unit);
        glClearColor(m_clear[0], m_clear[1], m_clear[2], m_clear[3]);
        if (m_scissor)
            glEnable(GL_SCISSOR_TEST);
        if (m_blend)
            glEnable(GL_BLEND);
    }

  private:
    GLint m_read, m_draw, m_vao, m_program, m_unit, m_texture, m_sampler;
    std::array<GLint, 4> m_viewport{};
    std::array<GLfloat, 4> m_clear{};
    bool m_scissor = glIsEnabled(GL_SCISSOR_TEST);
    bool m_blend = glIsEnabled(GL_BLEND);
};

Vector2D processedSize(const Vector2D& source, const Vector2D& destination, int limit) {
    constexpr double maxPixels = 3840. * 2160.;
    const double w = destination.x;
    const double h = destination.y;
    const auto factor = std::min({1., 2. * source.x / w, 2. * source.y / h,
        std::sqrt(maxPixels / (w * h)), double(limit) / std::max(w, h)});
    return {std::floor(w * factor), std::floor(h * factor)};
}

bool allocate(CFramebuffer& framebuffer, const Vector2D& size) {
    return framebuffer.m_size == size || framebuffer.alloc(size.x, size.y, DRM_FORMAT_ABGR8888);
}

}

Upscaler::Upscaler(Filter filter) : m_filter(filter) {}

Upscaler::~Upscaler() {
    if (!m_vao)
        return;
    g_pHyprRenderer->makeEGLCurrent();
    const DrawState state;
    m_capture.release();
    m_source.release();
    m_intermediate.release();
    m_output.release();
    glDeleteVertexArrays(1, &m_vao);
    glDeleteProgram(m_upscale.id);
    glDeleteProgram(m_sharpen.id);
}

bool Upscaler::Program::compile(const std::string& algorithm) {
    id = g_pHyprOpenGL->createProgram(vertex, fragment + algorithm, true);
    if (!id)
        return false;
    sampler = glGetUniformLocation(id, "inputImage");
    size = glGetUniformLocation(id, "outputSize");
    sharpness = glGetUniformLocation(id, "sharpness");
    return true;
}

void Upscaler::Program::draw(CFramebuffer& destination, const SP<CTexture>& texture) const {
    glBindFramebuffer(GL_DRAW_FRAMEBUFFER, destination.getFBID());
    glViewport(0, 0, destination.m_size.x, destination.m_size.y);
    glUseProgram(id);
    texture->bind();
    glUniform1i(sampler, 0);
    glUniform2f(size, destination.m_size.x, destination.m_size.y);
    if (sharpness >= 0)
        glUniform1f(sharpness, 0.8f);
    glDrawArrays(GL_TRIANGLES, 0, 3);
}

bool Upscaler::initialize() {
    if (m_vao)
        return m_upscale.id && (m_filter != Filter::Fsr || m_sharpen.id);
    GLint major = 0, minor = 0;
    glGetIntegerv(GL_MAJOR_VERSION, &major);
    glGetIntegerv(GL_MINOR_VERSION, &minor);
    if (major < 3 || (major == 3 && minor < 1))
        return false;
    glGetIntegerv(GL_MAX_TEXTURE_SIZE, &m_maxTexture);
    glGenVertexArrays(1, &m_vao);
    if (m_filter == Filter::Fsr)
        return m_upscale.compile(std::string("#define FSR_EASU_F\n") + Shaders::fsr + Shaders::easu)
            && m_sharpen.compile(std::string("#define FSR_RCAS_F\n") + Shaders::fsr + Shaders::rcas);
    return m_upscale.compile(Shaders::sgsr);
}

bool Upscaler::prepare(const Vector2D& monitor, const Vector2D& source, const CBox& destination) {
    if (!initialize() || source.x < 1 || source.y < 1 || source.x > monitor.x || source.y > monitor.y
        || monitor.x > m_maxTexture || monitor.y > m_maxTexture)
        return false;
    const auto extent = processedSize(source, destination.size(), m_maxTexture);
    if (extent.x < source.x || extent.y < source.y || extent == source)
        return false;

    const DrawState state;
    if (!allocate(m_capture, monitor) || !allocate(m_source, source) || !allocate(m_output, extent)
        || (m_filter == Filter::Fsr && !allocate(m_intermediate, extent)))
        return false;
    m_destination = destination;
    return true;
}

void Upscaler::begin() {
    auto& render = g_pHyprOpenGL->m_renderData;
    m_previous = render.currentFB;
    render.currentFB = &m_capture;
    m_capture.bind();
    const DrawState state;
    glClearColor(0, 0, 0, 1);
    glClear(GL_COLOR_BUFFER_BIT);
}

bool Upscaler::finish(const CRegion& damage) {
    if (!m_previous)
        return false;
    auto& render = g_pHyprOpenGL->m_renderData;
    render.currentFB = std::exchange(m_previous, nullptr);
    render.currentFB->bind();

    {
        const DrawState state;
        glBindFramebuffer(GL_READ_FRAMEBUFFER, m_capture.getFBID());
        glBindFramebuffer(GL_DRAW_FRAMEBUFFER, m_source.getFBID());
        const int w = m_source.m_size.x;
        const int h = m_source.m_size.y;
        // Hyprland's monitor projection puts the top edge at GL y=0.
        glBlitFramebuffer(0, 0, w, h, 0, 0, w, h, GL_COLOR_BUFFER_BIT, GL_NEAREST);

        glBindVertexArray(m_vao);
        if (m_filter == Filter::Fsr) {
            m_upscale.draw(m_intermediate, m_source.getTexture());
            m_sharpen.draw(m_output, m_intermediate.getTexture());
        } else {
            m_upscale.draw(m_output, m_source.getTexture());
        }
    }

    g_pHyprOpenGL->clear(CHyprColor{0., 0., 0., 1.});
    g_pHyprOpenGL->renderTexture(m_output.getTexture(), m_destination, {.damage = &damage});
    return true;
}

}
