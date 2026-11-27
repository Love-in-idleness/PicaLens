// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <input.h>

#include <QMetaObject>
#include <QPointer>
#include <QRectF>
#include <QSet>

#include <chrono>
#include <optional>

namespace KWin
{

class SeatInterface;
class Window;

class PicaLensInputFilter : public InputEventFilter
{
public:
    PicaLensInputFilter();
    ~PicaLensInputFilter() override;

    // All rectangles use KWin's global logical coordinates. Enable after
    // painting the source into exactly this picture rectangle.
    void setPresentation(Window *target, const QRectF &output, const QRectF &picture, const QRectF &source);
    void blockOutput(const QRectF &output);
    void clear();

    bool pointerMotion(PointerMotionEvent *event) override;
    bool pointerButton(PointerButtonEvent *event) override;
    bool pointerAxis(PointerAxisEvent *event) override;

private:
    enum class Route {
        Desktop,
        Blocked,
        Source,
    };

    Route routePointer(const QPointF &position, std::chrono::microseconds time);
    bool canRoute(SeatInterface *seat) const;
    void releaseClaim(bool restoreFocus);

    struct Click {
        quint32 button;
        QPointF start;
    };

    QPointer<Window> m_target;
    QRectF m_output;
    QRectF m_picture;
    QRectF m_source;
    QSet<quint32> m_interceptedButtons;
    QSet<quint32> m_physicalReleases;
    std::optional<Click> m_click;
    QMetaObject::Connection m_buttonObservation;
    bool m_ownsFocus = false;
    bool m_waitForButtonsUp = false;
};

}
