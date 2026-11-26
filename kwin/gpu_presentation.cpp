// SPDX-License-Identifier: GPL-2.0-or-later
#include "gpu_presentation.h"

#include "core/rendertarget.h"
#include "core/renderviewport.h"
#include "effect/effecthandler.h"
#include "effect/effectwindow.h"
#include "opengl/glframebuffer.h"
#include "opengl/gltexture.h"
#include "scene/surfaceitem.h"
#include "scene/windowitem.h"

#include <QFile>
#include <QRegion>

#include <algorithm>
#include <array>
#include <cmath>

namespace PicaLens
{
namespace
{

constexpr auto Vertex = R"(
void main() {
    vec2 corner = vec2((gl_VertexID << 1) & 2, gl_VertexID & 2);
    gl_Position = vec4(corner * 2.0 - 1.0, 0.0, 1.0);
}
)";
constexpr auto Fragment = R"(
uniform sampler2D inputImage;
uniform vec2 outputSize;
out vec4 outputColor;
)";
constexpr auto Present = R"(
uniform vec2 origin;
uniform mat2 inverseAxes;
void main() {
    vec2 uv = inverseAxes * (gl_FragCoord.xy - origin);
    outputColor = texture(inputImage, vec2(uv.x, 1.0 - uv.y));
}
)";

QByteArray shaderSource(const char *name)
{
    QFile file(QByteArray(":/picalens/shaders/") + name);
    return file.open(QIODevice::ReadOnly) ? file.readAll() : QByteArray();
}

GLuint compile(GLenum kind, const QByteArray &source)
{
    const GLuint shader = glCreateShader(kind);
    const char *text = source.constData();
    glShaderSource(shader, 1, &text, nullptr);
    glCompileShader(shader);
    GLint success;
    glGetShaderiv(shader, GL_COMPILE_STATUS, &success);
    if (success) {
        return shader;
    }
    std::array<GLchar, 2048> log{};
    glGetShaderInfoLog(shader, log.size(), nullptr, log.data());
    qWarning("picalens shader compilation failed: %s", log.data());
    glDeleteShader(shader);
    return 0;
}

GLuint link(const QByteArray &version, const QByteArray &fragment)
{
    const GLuint vs = compile(GL_VERTEX_SHADER, version + Vertex);
    const GLuint fs = compile(GL_FRAGMENT_SHADER, version + Fragment + fragment);
    GLuint program = 0;
    if (vs && fs) {
        program = glCreateProgram();
        glAttachShader(program, vs);
        glAttachShader(program, fs);
        glLinkProgram(program);
        GLint success;
        glGetProgramiv(program, GL_LINK_STATUS, &success);
        if (!success) {
            std::array<GLchar, 2048> log{};
            glGetProgramInfoLog(program, log.size(), nullptr, log.data());
            qWarning("picalens shader linking failed: %s", log.data());
            glDeleteProgram(program);
            program = 0;
        }
    }
    glDeleteShader(vs);
    glDeleteShader(fs);
    return program;
}

struct Program {
    Program() = default;
    Program(const Program &) = delete;
    Program &operator=(const Program &) = delete;
    ~Program() { glDeleteProgram(id); }

    void load(const QByteArray &version, const QByteArray &fragment)
    {
        id = link(version, fragment);
        if (id) {
            sampler = glGetUniformLocation(id, "inputImage");
            size = glGetUniformLocation(id, "outputSize");
        }
    }

    void use(GLuint texture, const QSize &extent) const
    {
        glUseProgram(id);
        glBindTexture(GL_TEXTURE_2D, texture);
        glUniform1i(sampler, 0);
        glUniform2f(size, extent.width(), extent.height());
    }

    GLuint id = 0;
    GLint sampler = -1;
    GLint size = -1;
};

struct Target {
    std::unique_ptr<KWin::GLTexture> texture;
    std::unique_ptr<KWin::GLFramebuffer> framebuffer;

    bool allocate(const QSize &size)
    {
        if (framebuffer && framebuffer->size() == size) {
            return true;
        }
        auto image = KWin::GLTexture::allocate(GL_RGBA8, size);
        if (!image) {
            return false;
        }
        image->setWrapMode(GL_CLAMP_TO_EDGE);
        image->setFilter(GL_LINEAR);
        auto fbo = std::make_unique<KWin::GLFramebuffer>(image.get());
        if (!fbo->valid()) {
            return false;
        }
        framebuffer = std::move(fbo);
        texture = std::move(image);
        return true;
    }

    GLuint id() const { return texture->texture(); }
};

// KWin tracks its own FBO stack. The native passes use it and restore the GL
// state that the scene still needs after this fullscreen effect.
class FramebufferBinding
{
public:
    explicit FramebufferBinding(KWin::GLFramebuffer *framebuffer)
    {
        KWin::GLFramebuffer::pushFramebuffer(framebuffer);
    }
    ~FramebufferBinding() { KWin::GLFramebuffer::popFramebuffer(); }
};

class DrawState
{
public:
    DrawState()
    {
        glGetIntegerv(GL_VIEWPORT, viewport.data());
        glGetIntegerv(GL_SCISSOR_BOX, scissor.data());
        glGetIntegerv(GL_CURRENT_PROGRAM, &program);
        glGetIntegerv(GL_VERTEX_ARRAY_BINDING, &vao);
        glGetIntegerv(GL_ACTIVE_TEXTURE, &activeTexture);
        glActiveTexture(GL_TEXTURE0);
        glGetIntegerv(GL_TEXTURE_BINDING_2D, &texture);
        glGetIntegerv(GL_SAMPLER_BINDING, &sampler);
        glGetBooleanv(GL_COLOR_WRITEMASK, colorMask.data());
        glGetFloatv(GL_COLOR_CLEAR_VALUE, clearColor.data());
        glBindSampler(0, 0);
    }

    ~DrawState()
    {
        glUseProgram(program);
        glBindVertexArray(vao);
        glActiveTexture(GL_TEXTURE0);
        glBindTexture(GL_TEXTURE_2D, texture);
        glBindSampler(0, sampler);
        glActiveTexture(activeTexture);
        glViewport(viewport[0], viewport[1], viewport[2], viewport[3]);
        glScissor(scissor[0], scissor[1], scissor[2], scissor[3]);
        glColorMask(colorMask[0], colorMask[1], colorMask[2], colorMask[3]);
        glClearColor(clearColor[0], clearColor[1], clearColor[2], clearColor[3]);
        for (const auto &entry : capabilities) {
            entry.enabled ? glEnable(entry.state) : glDisable(entry.state);
        }
    }

    void prepare() const
    {
        glActiveTexture(GL_TEXTURE0);
        glBindSampler(0, 0);
        for (const auto &entry : capabilities) {
            glDisable(entry.state);
        }
        glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
        glClearColor(0, 0, 0, 1);
    }

private:
    struct Capability {
        GLenum state;
        bool enabled;
    };
    std::array<Capability, 5> capabilities{{
        {GL_BLEND, bool(glIsEnabled(GL_BLEND))},
        {GL_SCISSOR_TEST, bool(glIsEnabled(GL_SCISSOR_TEST))},
        {GL_CULL_FACE, bool(glIsEnabled(GL_CULL_FACE))},
        {GL_DEPTH_TEST, bool(glIsEnabled(GL_DEPTH_TEST))},
        {GL_STENCIL_TEST, bool(glIsEnabled(GL_STENCIL_TEST))},
    }};
    std::array<GLint, 4> viewport{}, scissor{};
    std::array<GLboolean, 4> colorMask{};
    std::array<GLfloat, 4> clearColor{};
    GLint program, vao, activeTexture, texture, sampler;
};

QSize extent(const QSize &source, const QSize &destination, int maxTexture)
{
    if (source.isEmpty() || destination.isEmpty()) {
        return {};
    }
    constexpr double maxPixels = 3840. * 2160.;
    const double w = destination.width();
    const double h = destination.height();
    const double scale = std::min({1., 2. * source.width() / w, 2. * source.height() / h,
                                   std::sqrt(maxPixels / (w * h)), double(maxTexture) / std::max(w, h)});
    const QSize size(int(std::floor(w * scale)), int(std::floor(h * scale)));
    if (size.width() < source.width() || size.height() < source.height() || size == source) {
        return {};
    }
    return size;
}

QPointF glPoint(const KWin::RenderViewport &viewport, const KWin::RenderTarget &target, QPointF logical)
{
    const auto point = viewport.mapToRenderTarget(logical);
    return QPointF(point.x(), target.size().height() - point.y());
}

}

struct GpuPresentation::Pipeline {
    explicit Pipeline(Filter filter) : filter(filter) {}
    ~Pipeline() { if (vao) glDeleteVertexArrays(1, &vao); }

    bool initialize()
    {
        if (vao) {
            return upscale.id && present.id && (filter != Filter::Fsr || sharpen.id);
        }
        const bool desktop = epoxy_is_desktop_gl();
        if (epoxy_gl_version() < (desktop ? 40 : 31)) {
            return false;
        }
        const QByteArray version = desktop ? "#version 400 core\n"
                                           : "#version 310 es\nprecision highp float;\nprecision highp int;\n";
        if (filter == Filter::Fsr) {
            const auto fsr = shaderSource("fsr1.glsl");
            upscale.load(version, "#define FSR_EASU_F\n" + fsr + shaderSource("easu-pass.glsl"));
            sharpen.load(version, "#define FSR_RCAS_F\n" + fsr + shaderSource("rcas-pass.glsl"));
        } else {
            upscale.load(version, shaderSource("sgsr1.glsl") + shaderSource("sgsr-pass.glsl"));
        }
        present.load(version, Present);
        glGenVertexArrays(1, &vao);
        glGetIntegerv(GL_MAX_TEXTURE_SIZE, &maxTexture);
        return vao && upscale.id && present.id && (filter != Filter::Fsr || sharpen.id);
    }

    void drawPass(const Program &program, GLuint input, const Target &destination, const QSize &size)
    {
        FramebufferBinding binding(destination.framebuffer.get());
        glViewport(0, 0, size.width(), size.height());
        program.use(input, size);
        glDrawArrays(GL_TRIANGLES, 0, 3);
    }

    Filter filter;
    Program upscale, sharpen, present;
    Target source, intermediate, output;
    GLuint vao = 0;
    GLint maxTexture = 0;
};

GpuPresentation::GpuPresentation(Filter filter) : m_pipeline(std::make_unique<Pipeline>(filter)) {}
GpuPresentation::~GpuPresentation() = default;

bool GpuPresentation::paint(KWin::EffectWindow *window, const Presentation &presentation,
                            const KWin::RenderTarget &target, const KWin::RenderViewport &viewport,
                            const Damage &damage)
{
    const auto *item = window ? window->windowItem() : nullptr;
    const auto *surface = item ? item->surfaceItem() : nullptr;
    if (!surface || !surface->isVisible() || !target.framebuffer()) {
        return false;
    }
#ifdef PICALENS_KWIN_67
    if (!surface->buffer() || !surface->texture()) {
        return false;
    }
#else
    const auto *pixmap = surface->pixmap();
    if (!pixmap || pixmap->isDiscarded() || !pixmap->texture()->isValid()) {
        return false;
    }
#endif
    const QSizeF pixels = surface->bufferSourceBox().size();
    const QSizeF logical = surface->destinationSize();
    if (pixels.isEmpty() || logical.isEmpty()) {
        return false;
    }
    const auto sourceScale = std::min(pixels.width() / logical.width(), pixels.height() / logical.height());
    const QSize sourceSize = (presentation.source.size() * sourceScale).toSize();
    const QSize destinationSize = (QSizeF(presentation.destination.size()) * viewport.scale()).toSize();
    auto &p = *m_pipeline;
    const DrawState state;
    if (!p.initialize()) {
        return false;
    }
    const QSize processed = extent(sourceSize, destinationSize, p.maxTexture);
    if (processed.isEmpty() || !p.source.allocate(sourceSize) || !p.output.allocate(processed)
        || (p.filter == Filter::Fsr && !p.intermediate.allocate(processed))) {
        return false;
    }

    const KWin::RenderTarget sourceTarget(p.source.framebuffer.get(), target.colorDescription());
#ifdef PICALENS_KWIN_67
    const KWin::RenderViewport sourceViewport(KWin::RectF(presentation.source), sourceScale, sourceTarget, QPoint());
    const Damage sourceDamage(KWin::Rect(paintRect(sourceViewport, presentation.source.toAlignedRect())));
#else
    const KWin::RenderViewport sourceViewport(presentation.source, sourceScale, sourceTarget);
    const Damage sourceDamage(paintRect(sourceViewport, presentation.source.toAlignedRect()));
#endif
    {
        FramebufferBinding binding(p.source.framebuffer.get());
        state.prepare();
        glViewport(0, 0, sourceSize.width(), sourceSize.height());
        glClear(GL_COLOR_BUFFER_BIT);
        KWin::WindowPaintData data;
        KWin::effects->renderWindow(sourceTarget, sourceViewport, window,
            KWin::Effect::PAINT_WINDOW_TRANSFORMED | KWin::Effect::PAINT_WINDOW_TRANSLUCENT,
            sourceDamage, data);
    }

    state.prepare();
    glBindVertexArray(p.vao);
    if (p.filter == Filter::Fsr) {
        p.drawPass(p.upscale, p.source.id(), p.intermediate, processed);
        p.drawPass(p.sharpen, p.intermediate.id(), p.output, processed);
    } else {
        p.drawPass(p.upscale, p.source.id(), p.output, processed);
    }

    const QRectF destination(presentation.destination);
    const QPointF origin = glPoint(viewport, target, destination.topLeft());
    const QPointF xAxis = glPoint(viewport, target, destination.topRight()) - origin;
    const QPointF yAxis = glPoint(viewport, target, destination.bottomLeft()) - origin;
    const double det = xAxis.x() * yAxis.y() - xAxis.y() * yAxis.x();
    if (std::abs(det) < 1.) {
        return false;
    }
    const GLfloat inverse[] = {float(yAxis.y() / det), float(-xAxis.y() / det),
                               float(-yAxis.x() / det), float(xAxis.x() / det)};

    FramebufferBinding binding(target.framebuffer());
    clearGlDamage(target, viewport, damage.intersected(paintRect(viewport, presentation.outputRect)));
    glViewport(0, 0, target.size().width(), target.size().height());
    glEnable(GL_SCISSOR_TEST);
    p.present.use(p.output.id(), processed);
    glUniform2f(glGetUniformLocation(p.present.id, "origin"), origin.x(), origin.y());
    glUniformMatrix2fv(glGetUniformLocation(p.present.id, "inverseAxes"), 1, GL_FALSE, inverse);
    const auto visible = damage.intersected(paintRect(viewport, presentation.destination));
#ifdef PICALENS_KWIN_67
    const auto clipped = visible.intersected(target.transformedRect());
    for (const auto &rect : clipped.rects()) {
        const QRect device = viewport.transform().map(rect, target.transformedSize());
#else
    for (const QRect &rect : visible) {
        const QRect device = viewport.mapToRenderTarget(rect);
#endif
        const QRect bounded = device.intersected(QRect(QPoint(), target.size()));
        if (!bounded.isEmpty()) {
            glScissor(bounded.x(), target.size().height() - bounded.y() - bounded.height(),
                      bounded.width(), bounded.height());
            glDrawArrays(GL_TRIANGLES, 0, 3);
        }
    }
    return true;
}

}
