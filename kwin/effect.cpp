// SPDX-License-Identifier: GPL-2.0-or-later

#include "gpu_presentation.h"
#include "inputfilter.h"
#include "presentation.h"

#include <QDBusConnection>
#include <QDBusConnectionInterface>
#include <QDBusContext>
#include <QDBusMessage>
#include <QDBusServiceWatcher>
#include <QDebug>
#include <QKeyEvent>
#include <QMouseEvent>
#include <QPointer>
#include <QTimer>

#include "cursor.h"
#include "cursorsource.h"
#include "effect/effect.h"
#include "effect/effecthandler.h"
#include "effect/effectwindow.h"
#include "input_event.h"
#include "pointer_input.h"
#include "wayland_server.h"
#include "window.h"

#ifdef PICALENS_KWIN_67
#include "core/renderviewport.h"
#include "scene/item.h"
#include "scene/itemrenderer.h"
#include "scene/workspacescene.h"
#endif

namespace KWin
{

static const QString backendPath = QStringLiteral("/io/github/LoveInIdleness/PicaLens/Backend");

static void repaintOutput(const QRect &rect)
{
    effects->addRepaint(rect.x(), rect.y(), rect.width(), rect.height());
}

class PicaLensEffect : public Effect, protected QDBusContext
{
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "io.github.LoveInIdleness.PicaLens.Backend1")

public:
    PicaLensEffect()
    {
        m_owner.setConnection(QDBusConnection::sessionBus());
        m_owner.setWatchMode(QDBusServiceWatcher::WatchForUnregistration);
        m_registered = QDBusConnection::sessionBus().registerObject(
            backendPath, this, QDBusConnection::ExportScriptableSlots | QDBusConnection::ExportScriptableSignals);
        if (!m_registered) {
            qWarning("picalens could not register its backend");
        }
        connect(&m_owner, &QDBusServiceWatcher::serviceUnregistered, this, [this](const QString &service) {
            if (m_owner.watchedServices().contains(service)) {
                stop("PicaLens disconnected");
            }
        });
        m_activationTimer.setSingleShot(true);
        connect(&m_activationTimer, &QTimer::timeout, this, [this] {
            stop("source activation timed out");
        });
        connect(effects, &EffectsHandler::windowActivated, this, [this](EffectWindow *active) {
            if (m_presentation && active != m_window) {
                stop("focus changed");
            } else if (active && active == m_pendingActivation) {
                start(active);
            }
        });
        connect(effects, &EffectsHandler::windowClosed, this, [this](EffectWindow *window) {
            if (window == m_window || window == m_pendingActivation) {
                stop("source closed");
            }
        });
        connect(effects, &EffectsHandler::windowAdded, this, [this](EffectWindow *window) {
            if (m_presentation && window != m_window
                && (window->isPopupWindow() || window->window()->transientFor() == m_source)) {
                stop("popup or transient opened");
            }
        });
        connect(effects, &EffectsHandler::screenLockingChanged, this, [this](bool locked) {
            if (locked) {
                stop("screen locked");
            }
        });
        connect(effects, &EffectsHandler::desktopChanged, this, [this] {
            stop("desktop changed");
        });
        connect(effects, &EffectsHandler::screenRemoved, this, [this] {
            stop("output removed");
        });
        connect(effects, &EffectsHandler::virtualScreenGeometryChanged, this, [this] {
            stop("output geometry changed");
        });
        connect(effects, &EffectsHandler::activeFullScreenEffectChanged, this, [this] {
            if ((m_selecting || m_pendingActivation || m_presentation)
                && effects->activeFullScreenEffect() != this) {
                stop("another fullscreen effect took over");
            }
        });
        auto *mouse = Cursors::self()->mouse();
        connect(mouse, &Cursor::cursorChanged, this, &PicaLensEffect::updateCursor);
        connect(mouse, &Cursor::posChanged, this, &PicaLensEffect::updateCursor);
        m_fallbackCursor.setShape(Qt::ArrowCursor);
        qInfo("picalens loaded");
    }

    ~PicaLensEffect() override
    {
        stop("effect unloaded");
        if (m_registered) {
            QDBusConnection::sessionBus().unregisterObject(backendPath);
        }
    }

    bool isActive() const override
    {
        return m_presentation.has_value();
    }

    bool blocksDirectScanout() const override
    {
        return isActive();
    }

#ifdef PICALENS_KWIN_67
    void prePaintScreen(ScreenPrePaintData &data) override
    {
        m_paintView = data.view;
        effects->prePaintScreen(data);
    }
#endif

    void paintScreen(const RenderTarget &target, const RenderViewport &viewport,
                     int mask, const PicaLens::Damage &damage, PicaLens::Screen *output) override
    {
        effects->paintScreen(target, viewport, mask, damage, output);
        if (!m_presentation || !m_window || output != m_presentation->output
            || !damage.intersects(PicaLens::paintRect(viewport, m_presentation->outputRect))) {
            return;
        }
        const bool painted = m_gpu
            ? m_gpu->paint(m_window, *m_presentation, target, viewport, damage)
            : PicaLens::paintPresentation(m_window, *m_presentation, target, viewport, damage);
        if (!painted) {
            if (!m_failedPaint) {
                m_failedPaint = true;
                QTimer::singleShot(0, this, [this] {
                    if (m_failedPaint) {
                        stop(m_gpu ? "interactive_filter_unavailable" : "presentation could not be drawn");
                    }
                });
            }
            return;
        }
#ifdef PICALENS_KWIN_67
        repaintSoftwareCursor(target, viewport, damage);
#endif
        if (!m_firstPaint && !m_failedPaint && damage.intersects(PicaLens::paintRect(viewport, m_presentation->destination))) {
            m_firstPaint = true;
            m_input.setPresentation(m_source, m_presentation->outputRect,
                                    m_presentation->destination, m_presentation->source);
            qInfo("picalens first frame drawn filter=%s", qPrintable(filterName(m_filter)));
        }
    }

#ifdef PICALENS_KWIN_67
    void pointerButton(PointerButtonEvent *event) override
    {
        if (event->state == PointerButtonState::Released) {
            chooseWindow(event->button, event->buttons, event->position);
        }
    }
#else
    void windowInputMouseEvent(QEvent *event) override
    {
        if (event->type() == QEvent::MouseButtonRelease) {
            const auto *mouse = static_cast<QMouseEvent *>(event);
            chooseWindow(mouse->button(), mouse->buttons(), mouse->globalPosition());
        }
    }
#endif

    void grabbedKeyboardEvent(QKeyEvent *event) override
    {
        if (m_selecting && event->type() == QEvent::KeyRelease && event->key() == Qt::Key_Escape) {
            stop("selection cancelled", true);
        }
    }

public Q_SLOTS:
    Q_SCRIPTABLE bool Start()
    {
        return startSelection(PicaLens::Layout::Fit);
    }

    Q_SCRIPTABLE QStringList SupportedLayouts() const
    {
        return {QStringLiteral("fit"), QStringLiteral("stretch"), QStringLiteral("fill")};
    }

    Q_SCRIPTABLE QStringList SupportedFilters() const
    {
        if (effects->isOpenGLCompositing()) {
            return {QStringLiteral("linear"), QStringLiteral("fsr"), QStringLiteral("sgsr")};
        }
        return {QStringLiteral("linear")};
    }

    Q_SCRIPTABLE bool StartWithLayout(const QString &layout)
    {
        const auto parsed = parseLayout(layout);
        return parsed && startSelection(*parsed);
    }

    Q_SCRIPTABLE bool StartActive(const QString &layout)
    {
        return startActive(layout, PicaLens::Filter::Linear);
    }

    Q_SCRIPTABLE bool StartWithFilter(const QString &layout, const QString &filter)
    {
        const auto parsed = parseLayout(layout);
        const auto selected = parseGpuFilter(filter);
        return parsed && selected && effects->isOpenGLCompositing() && startSelection(*parsed, *selected);
    }

    Q_SCRIPTABLE bool StartActiveWithFilter(const QString &layout, const QString &filter)
    {
        const auto selected = parseGpuFilter(filter);
        return selected && effects->isOpenGLCompositing() && startActive(layout, *selected);
    }

    Q_SCRIPTABLE void Stop()
    {
        if (calledFromDBus() && m_owner.watchedServices().contains(message().service())) {
            stop("PicaLens requested stop");
        }
    }

Q_SIGNALS:
    Q_SCRIPTABLE void Finished(const QString &reason, bool returnToControls);

private:
    bool startActive(const QString &layout, PicaLens::Filter filter)
    {
        const auto parsed = parseLayout(layout);
        if (!parsed || !canStart()) {
            return false;
        }
        const auto appId = QStringLiteral("io.github.LoveInIdleness.PicaLens");
        const auto owner = connection().interface()->serviceOwner(appId);
        auto *window = effects->activeWindow();
        if (!owner.isValid() || owner.value() != message().service() || !window || !eligible(window)
            || window->window()->desktopFileName() == appId || window->window()->resourceClass() == appId) {
            return false;
        }
        m_owner.addWatchedService(message().service());
        m_layout = *parsed;
        m_filter = filter;
        return start(window);
    }

    static QString filterName(PicaLens::Filter filter)
    {
        switch (filter) {
        case PicaLens::Filter::Fsr: return QStringLiteral("fsr");
        case PicaLens::Filter::Sgsr: return QStringLiteral("sgsr");
        case PicaLens::Filter::Linear: return QStringLiteral("linear");
        }
        Q_UNREACHABLE_RETURN(QString());
    }

    static std::optional<PicaLens::Filter> parseGpuFilter(const QString &name)
    {
        for (const auto filter : {PicaLens::Filter::Fsr, PicaLens::Filter::Sgsr}) {
            if (filterName(filter) == name) {
                return filter;
            }
        }
        return std::nullopt;
    }

    static std::optional<PicaLens::Layout> parseLayout(const QString &layout)
    {
        if (layout == QStringLiteral("stretch")) {
            return PicaLens::Layout::Stretch;
        }
        if (layout == QStringLiteral("fill")) {
            return PicaLens::Layout::Fill;
        }
        if (layout == QStringLiteral("fit")) {
            return PicaLens::Layout::Fit;
        }
        return std::nullopt;
    }

    bool canStart() const
    {
        return calledFromDBus() && m_owner.watchedServices().isEmpty()
            && !effects->isScreenLocked() && !effects->activeFullScreenEffect()
            && !effects->isMouseInterception() && !input()->isSelectingWindow();
    }

    bool startSelection(PicaLens::Layout layout, PicaLens::Filter filter = PicaLens::Filter::Linear)
    {
        if (!canStart() || !effects->grabKeyboard(this)) {
            return false;
        }
        m_owner.addWatchedService(message().service());
        m_layout = layout;
        m_filter = filter;
        m_selecting = true;
        effects->startMouseInterception(this, Qt::CrossCursor);
        qInfo("picalens selecting an existing window");
        return true;
    }

    void updateCursor()
    {
        auto *mouse = Cursors::self()->mouse();
        auto *source = mouse->source();
        const bool showingFallback = source == &m_fallbackCursor;
        if (showingFallback) {
            source = m_originalCursor.data();
        }
        const bool useFallback = m_presentation && QRectF(m_presentation->outputRect).contains(mouse->pos())
            && (!source || source->isBlank()) && !m_fallbackCursor.isBlank();
        if (showingFallback && useFallback) {
            return;
        }

        if (m_originalCursorChanged) {
            disconnect(m_originalCursorChanged);
            m_originalCursorChanged = {};
        }
        m_originalCursor.clear();
        if (useFallback) {
            // Keep watching the client cursor while KWin displays the arrow.
            m_originalCursor = source;
            if (source) {
                m_originalCursorChanged = connect(source, &CursorSource::changed, this, &PicaLensEffect::updateCursor);
            }
            mouse->setSource(&m_fallbackCursor);
        } else if (showingFallback) {
            mouse->setSource(source);
        }
    }

#ifdef PICALENS_KWIN_67
    void repaintSoftwareCursor(const RenderTarget &target, const RenderViewport &viewport,
                               const Region &damage)
    {
        auto *view = m_paintView.data();
        auto *scene = effects->scene();
        auto *cursor = scene->cursorItem();
        if (!view || !cursor->isVisible() || !view->shouldRenderItem(cursor)) {
            return;
        }

        // The scene has already painted its software cursor. Redraw only the
        // pixels the presentation covered, with the original size and hotspot.
        const auto bounds = viewport.mapToDeviceCoordinatesAligned(cursor->mapToScene(cursor->boundingRect()));
        const auto repaint = damage.intersected(PicaLens::paintRect(viewport, m_presentation->outputRect))
                                 .intersected(bounds);
        if (!repaint.isEmpty()) {
            scene->renderer()->renderItem(target, viewport, cursor, PAINT_SCREEN_TRANSFORMED,
                                          repaint, WindowPaintData{}, [view](Item *item) {
                return !view->shouldRenderItem(item);
            }, [view](Item *item) {
                return view->shouldRenderHole(item);
            });
        }
    }
#endif

    void chooseWindow(Qt::MouseButton button, Qt::MouseButtons buttons, const QPointF &position)
    {
        if (!m_selecting) {
            return;
        }
        if (button == Qt::RightButton) {
            stop("selection cancelled", true);
        } else if (button == Qt::LeftButton && buttons == Qt::NoButton) {
            endSelection();
            // KWin suppresses normal hit testing during mouse interception.
            auto *window = input()->findToplevel(position);
            auto *effectWindow = window ? window->effectWindow() : nullptr;
            if (effectWindow && eligible(effectWindow)) {
                activate(effectWindow);
            } else {
                stop("no eligible window selected", true);
            }
        }
    }

    void endSelection()
    {
        if (!m_selecting) {
            return;
        }
        m_selecting = false;
        effects->stopMouseInterception(this);
        effects->ungrabKeyboard();
    }

    bool eligible(EffectWindow *window) const
    {
        return !effects->isScreenLocked() && !effects->activeFullScreenEffect()
            && window->isNormalWindow() && !window->isPopupWindow() && !window->isDeleted()
            && !window->isMinimized() && window->isOnCurrentDesktop()
            && (window->isWaylandClient() || window->isX11Client());
    }

    void activate(EffectWindow *window)
    {
        m_pendingActivation = window;
        m_activationTimer.start(1500);
        // Let KWin finish dispatching the selection click before moving focus.
        // XWayland then confirms activation asynchronously.
        QTimer::singleShot(0, this, [this, selected = QPointer(window)] {
            if (!selected || m_pendingActivation != selected) {
                return;
            }
            effects->activateWindow(selected);
            if (m_pendingActivation == selected && effects->activeWindow() == selected) {
                start(selected);
            }
        });
    }

    bool start(EffectWindow *window)
    {
        m_pendingActivation.clear();
        m_activationTimer.stop();
        if (!eligible(window)) {
            stop("selected window is no longer eligible");
            return false;
        }
        auto *source = window->window();
        auto presentation = PicaLens::presentationFor(window, m_layout);
        if (!source->surface() || !presentation) {
            stop("window has no usable presentation");
            return false;
        }

        m_window = window;
        m_source = source;
        m_fallbackCursor.setTheme(input()->pointer()->cursorTheme());
        m_presentation = presentation;
        if (m_filter != PicaLens::Filter::Linear) {
            m_gpu = std::make_unique<PicaLens::GpuPresentation>(m_filter);
        }
        source->refOffscreenRendering();
        connect(window, &EffectWindow::windowDamaged, this, [this] {
            repaintOutput(m_presentation->outputRect);
        });
        connect(window, &EffectWindow::windowFrameGeometryChanged, this, [this] {
            stop("source geometry changed");
        });
        connect(source, &Window::clientGeometryChanged, this, [this] {
            stop("source client geometry changed");
        });
        connect(source, &Window::bufferGeometryChanged, this, [this] {
            stop("source buffer geometry changed");
        });
        connect(source, &Window::outputChanged, this, [this] {
            stop("source output changed");
        });
        connect(source, &Window::minimizedChanged, this, [this] {
            stop("source minimized");
        });
        connect(window, &QObject::destroyed, this, [this] {
            stop("source destroyed");
        });
        effects->setActiveFullScreenEffect(this);
        updateCursor();
        repaintOutput(presentation->outputRect);
        qInfo().nospace() << "picalens started protocol=" << (window->isX11Client() ? "xwayland" : "wayland")
                          << " source=" << presentation->source << " destination="
                          << presentation->destination << " output=" << presentation->outputRect;
        return true;
    }

    void stop(const char *reason, bool returnToControls = false)
    {
        if (m_owner.watchedServices().isEmpty()) {
            return;
        }
        m_owner.setWatchedServices({});
        endSelection();
        m_activationTimer.stop();
        m_pendingActivation.clear();
        clearPresentation();
        qInfo("picalens stopped: %s", reason);
        Q_EMIT Finished(QString::fromLatin1(reason), returnToControls);
    }

    void clearPresentation()
    {
        if (!m_presentation) {
            return;
        }
        const QRect repaint = m_presentation->outputRect;
        m_presentation.reset();
        if (m_gpu) {
            const bool current = effects->makeOpenGLContextCurrent();
            m_gpu.reset();
            if (current) {
                effects->doneOpenGLContextCurrent();
            }
        }
        m_firstPaint = false;
        m_failedPaint = false;
        m_input.clear();
        updateCursor();
        if (m_window) {
            disconnect(m_window, nullptr, this, nullptr);
            m_window.clear();
        }
        if (m_source) {
            disconnect(m_source, nullptr, this, nullptr);
            m_source->unrefOffscreenRendering();
            m_source.clear();
        }
        if (effects->activeFullScreenEffect() == this) {
            effects->setActiveFullScreenEffect(nullptr);
        }
        repaintOutput(repaint);
    }

    QPointer<EffectWindow> m_window;
    QPointer<EffectWindow> m_pendingActivation;
    QPointer<Window> m_source;
#ifdef PICALENS_KWIN_67
    QPointer<RenderView> m_paintView;
#endif
    QTimer m_activationTimer;
    QDBusServiceWatcher m_owner;
    ShapeCursorSource m_fallbackCursor;
    QPointer<CursorSource> m_originalCursor;
    QMetaObject::Connection m_originalCursorChanged;
    std::optional<PicaLens::Presentation> m_presentation;
    PicaLensInputFilter m_input;
    PicaLens::Layout m_layout = PicaLens::Layout::Fit;
    PicaLens::Filter m_filter = PicaLens::Filter::Linear;
    std::unique_ptr<PicaLens::GpuPresentation> m_gpu;
    bool m_registered = false;
    bool m_selecting = false;
    bool m_firstPaint = false;
    bool m_failedPaint = false;
};

KWIN_EFFECT_FACTORY_SUPPORTED_ENABLED(PicaLensEffect, "metadata.json",
                                      return waylandServer() != nullptr;,
                                      return false;)

} // namespace KWin

#include "effect.moc"
