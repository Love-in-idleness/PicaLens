// SPDX-License-Identifier: GPL-2.0-or-later
#include "inputfilter.h"

#include <input_event.h>
#include <pointer_input.h>
#include <wayland/pointer.h>
#include <wayland/seat.h>
#include <wayland/surface.h>
#include <wayland_server.h>
#include <window.h>
#include <workspace.h>

#include <QGuiApplication>
#include <QLineF>
#include <QMatrix4x4>
#include <QStyleHints>

#include <algorithm>
#include <utility>

namespace KWin
{
namespace
{

bool containsPoint(const QRectF &rect, const QPointF &point)
{
    return point.x() >= rect.left() && point.x() < rect.right()
        && point.y() >= rect.top() && point.y() < rect.bottom();
}

bool canRestoreFocus(SeatInterface *seat)
{
    return !waylandServer()->isScreenLocked() && !seat->isDrag()
        && !seat->isTouchSequence() && input() && !input()->isSelectingWindow();
}

void restoreDesktopFocus(SeatInterface *seat)
{
    if (!canRestoreFocus(seat)) {
        return;
    }
    auto *pointer = input()->pointer();
    if (pointer->areButtonsPressed()) {
        return;
    }
    pointer->update();
    Window *focus = pointer->focus();
    if (!focus || !focus->surface() || !focus->surface()->isMapped()) {
        if (seat->focusedPointerSurface()) {
            seat->notifyPointerLeave();
        }
        return;
    }

    const QMatrix4x4 transform = focus->inputTransformation();
    if (seat->focusedPointerSurface() != focus->surface()
        || seat->focusedPointerSurfaceTransformation() != transform) {
        seat->notifyPointerEnter(focus->surface(), pointer->pos(), transform);
        const auto [surface, point] = focus->surface()->mapToInputSurface(transform.map(pointer->pos()));
        if (seat->pointer() && seat->pointer()->focusedSurface() == surface) {
            seat->pointer()->sendMotion(point);
            seat->notifyPointerFrame();
        }
    }
}

} // namespace

PicaLensInputFilter::PicaLensInputFilter()
    // KWin 6.3.6 inserts ties before the built-in filter. Keep lock, drag,
    // selection, shortcuts and popup precedence, and run before Decoration.
    : InputEventFilter(InputFilterOrder::Decoration)
{
    auto *redirection = input();
    redirection->installInputEventFilter(this);
    // A higher-priority grab can consume a physical release before our filter
    // sees it. A later press of that button must start a new sequence.
    m_buttonObservation = QObject::connect(redirection, &InputRedirection::pointerButtonStateChanged,
                                           redirection, [this](quint32 button, PointerButtonState state) {
        if (state == PointerButtonState::Released && m_interceptedButtons.contains(button)) {
            m_physicalReleases.insert(button);
        } else if (state == PointerButtonState::Pressed && m_physicalReleases.remove(button)) {
            m_interceptedButtons.remove(button);
            if (m_click && m_click->button == button) {
                m_click.reset();
            }
        }
    });
}

PicaLensInputFilter::~PicaLensInputFilter()
{
    // The owner normally clears the presentation while it can still swallow
    // the physical releases. This is also safe when KWin is shutting down.
    QObject::disconnect(m_buttonObservation);
    clear();
}

void PicaLensInputFilter::setPresentation(Window *target, const QRectF &output, const QRectF &picture, const QRectF &source)
{
    if (m_target == target && m_output == output && m_picture == picture && m_source == source) {
        return;
    }
    clear();
    if (!target || output.isEmpty() || source.isEmpty() || picture.isEmpty() || !output.contains(picture)) {
        return;
    }
    m_target = target;
    m_output = output;
    m_picture = picture;
    m_source = source;
    m_waitForButtonsUp = input()->pointer()->areButtonsPressed();
}

void PicaLensInputFilter::clear()
{
    m_waitForButtonsUp = false;
    releaseClaim(true);
    m_target.clear();
    m_output = {};
    m_picture = {};
    m_source = {};
}

bool PicaLensInputFilter::canRoute(SeatInterface *seat) const
{
    auto *target = m_target.data();
    if (!seat || !target || !canRestoreFocus(seat) || !workspace()
        || workspace()->activeWindow() != target
        || !target->isShown() || !target->readyForPainting()
        || !target->isOnCurrentDesktop() || !target->isOnCurrentActivity()
        || target->isInteractiveMove() || target->isInteractiveResize()) {
        return false;
    }

    auto *surface = target->surface();
    // KWin gives a grabbed Wayland popup its own keyboard surface before
    // invoking lower input filters. Require the selected source itself.
    if (!surface || !surface->isMapped() || seat->focusedKeyboardSurface() != surface
        || surface->lockedPointer() || surface->confinedPointer()) {
        return false;
    }
    const auto &transients = target->transients();
    if (std::any_of(transients.cbegin(), transients.cend(), [](const Window *window) {
            return window->isShown() && window->isOnCurrentDesktop() && window->isOnCurrentActivity();
        })) {
        return false;
    }

    // Internal UI and popup surfaces retain their normal input path.
    auto *hover = input()->pointer()->hover();
    return !hover || hover == target
        || (!hover->isInternal() && !hover->isPopupWindow() && !hover->hasPopupGrab()
            && !hover->isLockScreen() && !hover->isLockScreenOverlay());
}

PicaLensInputFilter::Route PicaLensInputFilter::routePointer(const QPointF &position, std::chrono::microseconds time)
{
    auto *seat = waylandServer() ? waylandServer()->seat() : nullptr;
    if (!seat || !canRestoreFocus(seat)) {
        releaseClaim(false);
        return Route::Desktop;
    }
    if (!m_target || !containsPoint(m_output, position)) {
        releaseClaim(true);
        return Route::Desktop;
    }
    if (m_waitForButtonsUp || !canRoute(seat)) {
        releaseClaim(false);
        return Route::Blocked;
    }
    // This mirrors the picture-input approach in kwin-effect-upscale, with
    // a shared capture/client rectangle and explicit input-region checks.
    // The transform order is display picture -> original window -> wl_surface.
    QMatrix4x4 pictureToWindow;
    pictureToWindow.translate(m_source.x(), m_source.y());
    pictureToWindow.scale(m_source.width() / m_picture.width(), m_source.height() / m_picture.height());
    pictureToWindow.translate(-m_picture.x(), -m_picture.y());
    const QMatrix4x4 transform = m_target->inputTransformation() * pictureToWindow;
    auto *surface = m_target->surface();
    const QPointF local = transform.map(position);
    if (!containsPoint(m_picture, position) || !surface->inputSurfaceAt(local)) {
        releaseClaim(false);
        if (seat->focusedPointerSurface()) {
            seat->notifyPointerLeave();
        }
        m_ownsFocus = true;
        return Route::Blocked;
    }

    m_ownsFocus = true;
    seat->setTimestamp(time);
    if (seat->focusedPointerSurface() != surface || seat->focusedPointerSurfaceTransformation() != transform) {
        // Re-enter with the actual mapped point. KWin sets pointerPos() on
        // enter and can suppress the subsequent motion at the same position.
        const auto [effective, point] = surface->mapToInputSurface(local);
        const bool sameEffective = seat->pointer() && seat->pointer()->focusedSurface() == effective;
        seat->notifyPointerEnter(surface, position, transform);
        if (sameEffective && seat->pointer()->focusedSurface() == effective) {
            seat->pointer()->sendMotion(point);
        }
    } else {
        seat->notifyPointerMotion(position);
    }
    return Route::Source;
}

void PicaLensInputFilter::releaseClaim(bool restoreFocus)
{
    m_click.reset();
    if (!std::exchange(m_ownsFocus, false)) {
        return;
    }
    auto *seat = waylandServer() ? waylandServer()->seat() : nullptr;
    if (!seat || !canRestoreFocus(seat)) {
        return;
    }
    auto *surface = m_target ? m_target->surface() : nullptr;
    if (surface && seat->focusedPointerSurface() == surface) {
        seat->notifyPointerLeave();
    }
    if (restoreFocus) {
        restoreDesktopFocus(seat);
    }
}

static bool movedBeyondClick(const QPointF &start, const QPointF &end)
{
    return QLineF(start, end).length() > QGuiApplication::styleHints()->startDragDistance();
}

bool PicaLensInputFilter::pointerMotion(PointerMotionEvent *event)
{
    if (!input()->pointer()->areButtonsPressed()) {
        m_waitForButtonsUp = false;
        m_interceptedButtons.subtract(m_physicalReleases);
        m_physicalReleases.clear();
        m_click.reset();
    } else if (m_interceptedButtons.isEmpty()) {
        m_waitForButtonsUp = true;
    }
    if (m_click && movedBeyondClick(m_click->start, event->position)) {
        m_click.reset();
    }
    return routePointer(event->position, event->timestamp) != Route::Desktop;
}

bool PicaLensInputFilter::pointerButton(PointerButtonEvent *event)
{
    const bool pressed = event->state == PointerButtonState::Pressed;
    if (!pressed) {
        m_physicalReleases.remove(event->nativeButton);
    }
    const bool intercepted = m_interceptedButtons.contains(event->nativeButton);
    if (!pressed && !intercepted) {
        if (!input()->pointer()->areButtonsPressed()) {
            m_waitForButtonsUp = false;
        }
        auto *seat = waylandServer() ? waylandServer()->seat() : nullptr;
        return m_target && containsPoint(m_output, event->position) && seat
            && canRestoreFocus(seat) && !seat->isPointerButtonPressed(event->nativeButton);
    }

    const auto route = routePointer(event->position, event->timestamp);
    if (pressed && route != Route::Desktop) {
        const bool firstButton = m_interceptedButtons.isEmpty();
        m_interceptedButtons.insert(event->nativeButton);
        m_click.reset();
        if (route == Route::Source && firstButton) {
            m_click = Click{event->nativeButton, event->position};
        }
    } else if (!pressed) {
        m_interceptedButtons.remove(event->nativeButton);
        if (m_click && m_click->button == event->nativeButton) {
            const auto click = *m_click;
            m_click.reset();
            if (route == Route::Source && !movedBeyondClick(click.start, event->position)) {
                // A cancelled press must never turn into a click when XWayland loses focus.
                auto *seat = waylandServer()->seat();
                seat->notifyPointerButton(click.button, PointerButtonState::Pressed);
                seat->notifyPointerButton(click.button, PointerButtonState::Released);
            }
        }
        if (!input()->pointer()->areButtonsPressed() && route == Route::Desktop && waylandServer()) {
            restoreDesktopFocus(waylandServer()->seat());
        }
    }
    if (route == Route::Desktop && pressed && !intercepted) {
        m_waitForButtonsUp = true;
    }
    return intercepted || route != Route::Desktop;
}

bool PicaLensInputFilter::pointerAxis(PointerAxisEvent *event)
{
    const auto route = routePointer(event->position, event->timestamp);
    if (route == Route::Source) {
        auto *seat = waylandServer()->seat();
        seat->notifyPointerAxis(event->orientation, event->delta, event->deltaV120, event->source, event->inverted);
    }
    return route != Route::Desktop;
}

}
