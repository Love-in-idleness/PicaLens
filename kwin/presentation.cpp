// SPDX-License-Identifier: GPL-2.0-or-later
#include "presentation.h"

#include "core/output.h"
#include "core/rendertarget.h"
#include "core/renderviewport.h"
#include "effect/effecthandler.h"
#include "effect/effectwindow.h"
#include "scene/surfaceitem.h"
#include "scene/windowitem.h"

#include <QPainter>
#include <QRegion>
#include <epoxy/gl.h>

#include <cmath>

namespace PicaLens
{

QRect paintRect([[maybe_unused]] const KWin::RenderViewport &viewport, const QRect &logicalRect)
{
#ifdef PICALENS_KWIN_67
    return viewport.mapToDeviceCoordinatesAligned(KWin::Rect(logicalRect));
#else
    return logicalRect;
#endif
}

std::optional<Presentation> presentationFor(KWin::EffectWindow *window, Layout layout)
{
    if (!window || window->isDeleted() || !window->isVisible() || !window->screen()) {
        return std::nullopt;
    }
    auto *output = window->screen();
    const QRect outputRect = output->geometry();
    // On XWayland the frame extents can retain their pre-scale values after
    // an output change. Keep the full client rectangle so its pixels survive.
    QRectF source = window->clientGeometry();
    if (source.isEmpty() || outputRect.isEmpty() || !window->bufferGeometry().contains(source)) {
        return std::nullopt;
    }

    QSize size = outputRect.size();
    if (layout == Layout::Fit) {
        QSizeF fit = source.size();
        fit.scale(size, Qt::KeepAspectRatio);
        size = fit.toSize();
    } else if (layout == Layout::Fill) {
        QSizeF visible(size);
        visible.scale(source.size(), Qt::KeepAspectRatio);
        const QPointF center = source.center();
        source.setSize(visible);
        source.moveCenter(center);
    }
    if (size.isEmpty()) {
        return std::nullopt;
    }
    // KWin's clip region has integer logical edges, so use the same rounded fit for hit testing.
    const QPoint origin(outputRect.x() + (outputRect.width() - size.width()) / 2,
                        outputRect.y() + (outputRect.height() - size.height()) / 2);
    return Presentation{output, outputRect, source, QRect(origin, size)};
}

static void clearGlDamage(const KWin::RenderTarget &target, const KWin::RenderViewport &viewport,
                          const Damage &damage)
{
    const bool hadScissor = glIsEnabled(GL_SCISSOR_TEST);
    GLint previousScissor[4];
    GLfloat previousColor[4];
    glGetIntegerv(GL_SCISSOR_BOX, previousScissor);
    glGetFloatv(GL_COLOR_CLEAR_VALUE, previousColor);
    glEnable(GL_SCISSOR_TEST);
    glClearColor(0, 0, 0, 1);
    const QRect bounds(QPoint(), target.size());
#ifdef PICALENS_KWIN_67
    const auto clipped = damage.intersected(target.transformedRect());
    for (const auto &rect : clipped.rects()) {
        const QRect buffer = viewport.transform().map(rect, target.transformedSize());
        const QRect device = buffer.intersected(bounds);
#else
    for (const QRect &rect : damage) {
        const QRect device = viewport.mapToRenderTarget(rect).intersected(bounds);
#endif
        if (!device.isEmpty()) {
            glScissor(device.x(), bounds.height() - device.y() - device.height(),
                      device.width(), device.height());
            glClear(GL_COLOR_BUFFER_BIT);
        }
    }
    glClearColor(previousColor[0], previousColor[1], previousColor[2], previousColor[3]);
    glScissor(previousScissor[0], previousScissor[1], previousScissor[2], previousScissor[3]);
    if (!hadScissor) {
        glDisable(GL_SCISSOR_TEST);
    }
}

bool paintPresentation(KWin::EffectWindow *window, const Presentation &presentation,
                       const KWin::RenderTarget &target, const KWin::RenderViewport &viewport,
                       const Damage &damage)
{
    const auto repaint = damage.intersected(paintRect(viewport, presentation.outputRect));
    const auto content = repaint.intersected(paintRect(viewport, presentation.destination));
    if (repaint.isEmpty() || !window || window->isDeleted()) {
        return false;
    }

    const auto *item = window->windowItem();
    const auto *surface = item ? item->surfaceItem() : nullptr;
    if (!surface || !surface->isVisible()) {
        return false;
    }
    const bool openGl = KWin::effects->isOpenGLCompositing();
    if (openGl && !target.framebuffer()) {
        return false;
    }
    QPainter *painter = openGl ? nullptr : KWin::effects->scenePainter();
    if (!openGl && (!painter || !painter->isActive())) {
        return false;
    }

    const qreal scaleX = presentation.destination.width() / presentation.source.width();
    const qreal scaleY = presentation.destination.height() / presentation.source.height();
    QPointF sourceOffset = presentation.source.topLeft();
    QPointF origin;
    if (openGl) {
        // GL applies the paint transform inside the WindowItem's frame translation.
        const QPointF frame = window->pos();
        const qreal dpr = viewport.scale();
        const auto snap = [dpr](const QPointF &p) {
            return QPointF(std::round(p.x() * dpr) / dpr, std::round(p.y() * dpr) / dpr);
        };
        const QPointF buffer = window->bufferGeometry().topLeft();
        origin = snap(frame);
        sourceOffset = snap(buffer - frame) + (presentation.source.topLeft() - buffer);
        clearGlDamage(target, viewport, repaint);
    } else {
        // QPainter applies the paint transform outside the WindowItem's frame translation.
        painter->save();
        painter->setCompositionMode(QPainter::CompositionMode_Source);
#ifdef PICALENS_KWIN_67
        for (const auto &rect : repaint.rects()) {
            painter->fillRect(QRectF(viewport.mapFromDeviceCoordinates(rect)), Qt::black);
        }
#else
        for (const QRect &rect : repaint) {
            painter->fillRect(rect, Qt::black);
        }
#endif
        painter->restore();
    }
    if (content.isEmpty()) {
        return true;
    }

    KWin::WindowPaintData paint;
    paint.setXScale(scaleX);
    paint.setYScale(scaleY);
    paint.setXTranslation(presentation.destination.x() - origin.x() - sourceOffset.x() * scaleX);
    paint.setYTranslation(presentation.destination.y() - origin.y() - sourceOffset.y() * scaleY);
    // Render just the source item after the normal scene, even if the scene occluded it.
    KWin::effects->renderWindow(target, viewport, window,
                                KWin::Effect::PAINT_WINDOW_TRANSFORMED | KWin::Effect::PAINT_WINDOW_TRANSLUCENT,
                                content, paint);

#ifdef PICALENS_KWIN_67
    return surface->buffer() && surface->texture();
#else
    const auto *pixmap = surface->pixmap();
    return pixmap && !pixmap->isDiscarded() && pixmap->texture()->isValid();
#endif
}

}
