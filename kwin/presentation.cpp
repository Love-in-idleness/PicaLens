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

std::optional<Presentation> presentationFor(KWin::EffectWindow *window)
{
    if (!window || window->isDeleted() || !window->isVisible() || !window->screen()) {
        return std::nullopt;
    }
    KWin::Output *output = window->screen();
    const QRect outputRect = output->geometry();
    // On XWayland the frame extents can retain their pre-scale values after
    // an output change. Keep the full client rectangle so its pixels survive.
    const QRectF source = window->clientGeometry();
    if (source.isEmpty() || outputRect.isEmpty() || !window->bufferGeometry().contains(source)) {
        return std::nullopt;
    }

    QSizeF fit = source.size();
    fit.scale(outputRect.size(), Qt::KeepAspectRatio);
    const QSize size = fit.toSize();
    if (size.isEmpty()) {
        return std::nullopt;
    }
    // KWin's clip region has integer logical edges, so use the same rounded fit for hit testing.
    const QPoint origin(outputRect.x() + (outputRect.width() - size.width()) / 2,
                        outputRect.y() + (outputRect.height() - size.height()) / 2);
    return Presentation{output, outputRect, source, QRect(origin, size)};
}

static void clearGlDamage(const KWin::RenderTarget &target, const KWin::RenderViewport &viewport,
                          const QRegion &damage)
{
    const bool hadScissor = glIsEnabled(GL_SCISSOR_TEST);
    GLint previousScissor[4];
    GLfloat previousColor[4];
    glGetIntegerv(GL_SCISSOR_BOX, previousScissor);
    glGetFloatv(GL_COLOR_CLEAR_VALUE, previousColor);
    glEnable(GL_SCISSOR_TEST);
    glClearColor(0, 0, 0, 1);
    const QRect bounds(QPoint(), target.size());
    for (const QRect &rect : damage) {
        const QRect device = viewport.mapToRenderTarget(rect).intersected(bounds);
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
                       const QRegion &damage)
{
    const QRegion repaint = damage.intersected(presentation.outputRect);
    const QRegion content = repaint.intersected(presentation.destination);
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
        for (const QRect &rect : repaint) {
            painter->fillRect(rect, Qt::black);
        }
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

    const auto *pixmap = surface->pixmap();
    return pixmap && !pixmap->isDiscarded() && pixmap->texture()->isValid();
}

}
