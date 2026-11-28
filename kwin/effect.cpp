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
#include <QVariantMap>

#include <algorithm>
#include <utility>

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
        m_renderPoll.setSingleShot(true);
        connect(&m_renderPoll, &QTimer::timeout, this, [this] {
            if (m_presentation) {
                repaintOutput(m_presentation->outputRect);
            }
        });
        m_cursorIdleTimer.setSingleShot(true);
        connect(&m_cursorIdleTimer, &QTimer::timeout, this, &PicaLensEffect::hideIdleCursor);
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
        connect(mouse, &Cursor::posChanged, this, &PicaLensEffect::onCursorMoved);
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
                        stop(m_gpu ? m_gpu->error() : "presentation could not be drawn");
                    }
                });
            }
            return;
        }
        if (!m_pendingScreenshotPath.isEmpty() && (!m_gpu || m_gpu->hasFrame())) {
            const QString path = std::exchange(m_pendingScreenshotPath, {});
            const QImage image = PicaLens::captureDestination(*m_presentation, target, viewport);
            const bool saved = !image.isNull() && image.save(path, "PNG");
            QTimer::singleShot(0, this, [this, path, saved] {
                Q_EMIT ScreenshotSaved(path, saved);
            });
        }
#ifdef PICALENS_KWIN_67
        repaintSoftwareCursor(target, viewport, damage);
#endif
        if (m_gpu && m_gpu->pending() && !m_renderPoll.isActive()) {
            m_renderPoll.start(8);
        }
        if (m_gpu && !m_gpu->hasFrame()) {
            return;
        }
        if (!m_firstPaint && !m_failedPaint && damage.intersects(PicaLens::paintRect(viewport, m_presentation->destination))) {
            finishRenderWait();
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
        if ((m_selecting || m_renderGrab) && event->type() == QEvent::KeyRelease && event->key() == Qt::Key_Escape) {
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
            QStringList filters{QStringLiteral("linear"), QStringLiteral("fsr"), QStringLiteral("sgsr")};
            if (PicaLens::GpuPresentation::workerAvailable()) {
                filters.append(QStringLiteral("anime4k"));
            }
            return filters;
        }
        return {QStringLiteral("linear")};
    }

    Q_SCRIPTABLE int RenderWorkerVersion() const
    {
        return effects->isOpenGLCompositing() && PicaLens::GpuPresentation::workerAvailable() ? 1 : 0;
    }

    Q_SCRIPTABLE int CapabilitiesVersion() const
    {
        return 2;
    }

    Q_SCRIPTABLE QString ActiveWindowAppId() const
    {
        if (!calledFromDBus() || !callerIsOwner()) {
            return {};
        }
        auto *window = effects->activeWindow();
        if (!window || !eligible(window) || isPicaLensWindow(window)) {
            return {};
        }
        return windowAppId(window);
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

    Q_SCRIPTABLE bool StartWithRenderDevice(const QString &layout, const QString &filter, const QString &device)
    {
        const auto parsed = parseLayout(layout);
        const auto selected = parseGpuFilter(filter);
        return parsed && selected && RenderWorkerVersion() == 1 && device.size() <= 512
            && startSelection(*parsed, *selected, device);
    }

    Q_SCRIPTABLE bool StartActiveWithRenderDevice(const QString &layout, const QString &filter, const QString &device)
    {
        const auto selected = parseGpuFilter(filter);
        return selected && RenderWorkerVersion() == 1 && device.size() <= 512
            && startActive(layout, *selected, device);
    }

    Q_SCRIPTABLE bool StartWithOptions(const QVariantMap &options)
    {
        const auto parsed = parseStartOptions(options);
        return parsed && startSelection(parsed->layout, parsed->filter, parsed->renderDevice,
                                        parsed->crop, parsed->hideCursorMs);
    }

    Q_SCRIPTABLE bool StartActiveWithOptions(const QVariantMap &options)
    {
        const auto parsed = parseStartOptions(options);
        return parsed && startActive(parsed->layout, parsed->filter, parsed->renderDevice,
                                     parsed->crop, parsed->hideCursorMs);
    }

    Q_SCRIPTABLE bool CaptureScreenshot(const QString &path)
    {
        if (!calledFromDBus()
            || (!m_owner.watchedServices().contains(message().service()) && !callerIsOwner())
            || !m_presentation || !m_firstPaint || path.isEmpty() || !m_pendingScreenshotPath.isEmpty()) {
            return false;
        }
        m_pendingScreenshotPath = path;
        repaintOutput(m_presentation->outputRect);
        return true;
    }

    Q_SCRIPTABLE void Stop()
    {
        if (calledFromDBus() && m_owner.watchedServices().contains(message().service())) {
            stop("PicaLens requested stop");
        }
    }

Q_SIGNALS:
    Q_SCRIPTABLE void Finished(const QString &reason, bool returnToControls);
    Q_SCRIPTABLE void ScreenshotSaved(const QString &path, bool ok);

private:
    struct StartOptions {
        PicaLens::Layout layout = PicaLens::Layout::Fit;
        PicaLens::Filter filter = PicaLens::Filter::Linear;
        QString renderDevice;
        PicaLens::Crop crop;
        int hideCursorMs = 0;
    };

    bool callerIsOwner() const
    {
        const auto appId = QStringLiteral("io.github.LoveInIdleness.PicaLens");
        const auto owner = connection().interface()->serviceOwner(appId);
        return owner.isValid() && owner.value() == message().service();
    }

    static bool isPicaLensWindow(EffectWindow *window)
    {
        const auto appId = QStringLiteral("io.github.LoveInIdleness.PicaLens");
        return window->window()->desktopFileName() == appId || window->window()->resourceClass() == appId;
    }

    static QString windowAppId(EffectWindow *window)
    {
        auto *source = window->window();
        if (!source->desktopFileName().isEmpty()) {
            return source->desktopFileName();
        }
        if (!source->resourceClass().isEmpty()) {
            return source->resourceClass();
        }
        return source->resourceName();
    }

    std::optional<StartOptions> parseStartOptions(const QVariantMap &options) const
    {
        StartOptions parsed;
        const auto layout = parseLayout(options.value(QStringLiteral("layout"), QStringLiteral("fit")).toString());
        if (!layout) {
            return std::nullopt;
        }
        parsed.layout = *layout;
        const QString filterNameStr = options.value(QStringLiteral("filter"), QStringLiteral("linear")).toString();
        if (filterNameStr == QStringLiteral("linear")) {
            parsed.filter = PicaLens::Filter::Linear;
        } else if (const auto gpuFilter = parseGpuFilter(filterNameStr)) {
            if (!effects->isOpenGLCompositing()) {
                return std::nullopt;
            }
            parsed.filter = *gpuFilter;
        } else {
            return std::nullopt;
        }
        parsed.renderDevice = options.value(QStringLiteral("renderDevice")).toString();
        if (parsed.renderDevice.size() > 512) {
            return std::nullopt;
        }
        const bool needsWorker = parsed.filter == PicaLens::Filter::Anime4k
            || (parsed.filter != PicaLens::Filter::Linear && !parsed.renderDevice.isEmpty());
        if (needsWorker && RenderWorkerVersion() != 1) {
            return std::nullopt;
        }
        auto clampInt = [&](const char *key, int maxValue) {
            return std::clamp(options.value(QString::fromLatin1(key)).toInt(), 0, maxValue);
        };
        parsed.crop = PicaLens::Crop{
            clampInt("cropLeft", 4096),
            clampInt("cropTop", 4096),
            clampInt("cropRight", 4096),
            clampInt("cropBottom", 4096),
        };
        parsed.hideCursorMs = clampInt("hideCursorMs", 60000);
        return parsed;
    }

    bool startActive(const QString &layout, PicaLens::Filter filter, const QString &device = {})
    {
        const auto parsed = parseLayout(layout);
        return parsed && startActive(*parsed, filter, device, {}, 0);
    }

    bool startActive(PicaLens::Layout layout, PicaLens::Filter filter, const QString &device,
                     PicaLens::Crop crop, int hideCursorMs)
    {
        if (!canStart()) {
            return false;
        }
        auto *window = effects->activeWindow();
        if (!callerIsOwner() || !window || !eligible(window) || isPicaLensWindow(window)) {
            return false;
        }
        m_owner.addWatchedService(message().service());
        m_layout = layout;
        m_filter = filter;
        m_renderDevice = device;
        m_crop = crop;
        m_hideCursorMs = hideCursorMs;
        return start(window);
    }

    static QString filterName(PicaLens::Filter filter)
    {
        switch (filter) {
        case PicaLens::Filter::Fsr: return QStringLiteral("fsr");
        case PicaLens::Filter::Sgsr: return QStringLiteral("sgsr");
        case PicaLens::Filter::Anime4k: return QStringLiteral("anime4k");
        case PicaLens::Filter::Linear: return QStringLiteral("linear");
        }
        Q_UNREACHABLE_RETURN(QString());
    }

    static std::optional<PicaLens::Filter> parseGpuFilter(const QString &name)
    {
        for (const auto filter : {PicaLens::Filter::Fsr, PicaLens::Filter::Sgsr, PicaLens::Filter::Anime4k}) {
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

    bool startSelection(PicaLens::Layout layout, PicaLens::Filter filter = PicaLens::Filter::Linear,
                        const QString &device = {}, PicaLens::Crop crop = {}, int hideCursorMs = 0)
    {
        if (!canStart() || !effects->grabKeyboard(this)) {
            return false;
        }
        m_owner.addWatchedService(message().service());
        m_layout = layout;
        m_filter = filter;
        m_renderDevice = device;
        m_crop = crop;
        m_hideCursorMs = hideCursorMs;
        m_selecting = true;
        effects->startMouseInterception(this, Qt::CrossCursor);
        qInfo("picalens selecting an existing window");
        return true;
    }

    void onCursorMoved()
    {
        auto *mouse = Cursors::self()->mouse();
        const QPointF pos = mouse->pos();
        if (pos != m_lastCursorPos) {
            m_lastCursorPos = pos;
            showIdleCursor();
            if (m_presentation && m_hideCursorMs > 0) {
                m_cursorIdleTimer.start(m_hideCursorMs);
            }
        }
        updateCursor();
    }

    void hideIdleCursor()
    {
        if (!m_presentation || m_hideCursorMs <= 0 || m_cursorHidden) {
            return;
        }
        auto *mouse = Cursors::self()->mouse();
        if (!QRectF(m_presentation->outputRect).contains(mouse->pos())) {
            return;
        }
        m_cursorHidden = true;
        effects->hideCursor();
    }

    void showIdleCursor()
    {
        if (!m_cursorHidden) {
            return;
        }
        m_cursorHidden = false;
        effects->showCursor();
    }

    void updateCursor()
    {
        if (m_cursorHidden) {
            return;
        }
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
        auto presentation = PicaLens::presentationFor(window, m_layout, m_crop);
        if (!source->surface() || !presentation) {
            stop(!m_crop.isEmpty() && source->surface() ? "invalid_crop" : "window has no usable presentation");
            return false;
        }

        m_window = window;
        m_source = source;
        m_fallbackCursor.setTheme(input()->pointer()->cursorTheme());
        m_presentation = presentation;
        source->refOffscreenRendering();
        if (m_filter != PicaLens::Filter::Linear) {
            m_gpu = std::make_unique<PicaLens::GpuPresentation>(m_filter, m_renderDevice, [this] {
                if (m_presentation) {
                    repaintOutput(m_presentation->outputRect);
                }
            });
            if (!m_gpu->hasFrame()) {
                m_input.blockOutput(m_presentation->outputRect);
                m_renderGrab = effects->grabKeyboard(this);
                if (!m_renderGrab) {
                    stop("render_worker_failed");
                    return false;
                }
            }
        }
        connect(window, &EffectWindow::windowDamaged, this, [this] {
            if (m_gpu) {
                m_gpu->sourceDamaged();
            }
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
        m_lastCursorPos = Cursors::self()->mouse()->pos();
        if (m_hideCursorMs > 0) {
            m_cursorIdleTimer.start(m_hideCursorMs);
        }
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
        m_pendingScreenshotPath.clear();
        m_cursorIdleTimer.stop();
        showIdleCursor();
        m_hideCursorMs = 0;
        m_crop = {};
        m_renderPoll.stop();
        finishRenderWait();
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

    void finishRenderWait()
    {
        if (m_renderGrab) {
            m_renderGrab = false;
            effects->ungrabKeyboard();
        }
    }

    QPointer<EffectWindow> m_window;
    QPointer<EffectWindow> m_pendingActivation;
    QPointer<Window> m_source;
#ifdef PICALENS_KWIN_67
    QPointer<RenderView> m_paintView;
#endif
    QTimer m_activationTimer;
    QTimer m_renderPoll;
    QTimer m_cursorIdleTimer;
    QDBusServiceWatcher m_owner;
    ShapeCursorSource m_fallbackCursor;
    QPointer<CursorSource> m_originalCursor;
    QMetaObject::Connection m_originalCursorChanged;
    std::optional<PicaLens::Presentation> m_presentation;
    PicaLensInputFilter m_input;
    PicaLens::Layout m_layout = PicaLens::Layout::Fit;
    PicaLens::Filter m_filter = PicaLens::Filter::Linear;
    PicaLens::Crop m_crop;
    QString m_renderDevice;
    QString m_pendingScreenshotPath;
    QPointF m_lastCursorPos;
    int m_hideCursorMs = 0;
    std::unique_ptr<PicaLens::GpuPresentation> m_gpu;
    bool m_registered = false;
    bool m_selecting = false;
    bool m_firstPaint = false;
    bool m_failedPaint = false;
    bool m_renderGrab = false;
    bool m_cursorHidden = false;
};

KWIN_EFFECT_FACTORY_SUPPORTED_ENABLED(PicaLensEffect, "metadata.json",
                                      return waylandServer() != nullptr;,
                                      return false;)

} // namespace KWin

#include "effect.moc"
