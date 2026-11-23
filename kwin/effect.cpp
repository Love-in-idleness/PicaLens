// SPDX-License-Identifier: GPL-2.0-or-later

#include "inputfilter.h"
#include "presentation.h"

#include <QDBusConnection>
#include <QDBusContext>
#include <QDBusMessage>
#include <QDBusServiceWatcher>
#include <QDebug>
#include <QKeyEvent>
#include <QMouseEvent>
#include <QPointer>
#include <QTimer>

#include "effect/effect.h"
#include "effect/effecthandler.h"
#include "effect/effectwindow.h"
#include "wayland_server.h"
#include "window.h"

namespace KWin
{

static const QString backendPath = QStringLiteral("/io/github/LoveInIdleness/PicaLens/Backend");

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

    void paintScreen(const RenderTarget &target, const RenderViewport &viewport,
                     int mask, const QRegion &damage, Output *output) override
    {
        effects->paintScreen(target, viewport, mask, damage, output);
        if (!m_presentation || !m_window || output != m_presentation->output
            || !damage.intersects(m_presentation->outputRect)) {
            return;
        }
        if (!PicaLens::paintPresentation(m_window, *m_presentation, target, viewport, damage)) {
            if (!m_failedPaint) {
                m_failedPaint = true;
                QTimer::singleShot(0, this, [this] {
                    if (m_failedPaint) {
                        stop("presentation could not be drawn");
                    }
                });
            }
            return;
        }
        if (!m_firstPaint && !m_failedPaint && damage.intersects(m_presentation->destination)) {
            m_firstPaint = true;
            m_input.setPresentation(m_source, m_presentation->outputRect,
                                    m_presentation->destination, m_presentation->source);
            qInfo("picalens first frame drawn");
        }
    }

    void windowInputMouseEvent(QEvent *event) override
    {
        if (!m_selecting || event->type() != QEvent::MouseButtonRelease) {
            return;
        }
        const auto *mouse = static_cast<QMouseEvent *>(event);
        if (mouse->button() == Qt::RightButton) {
            stop("selection cancelled", true);
        } else if (mouse->button() == Qt::LeftButton && mouse->buttons() == Qt::NoButton) {
            endSelection();
            // KWin suppresses normal hit testing during mouse interception.
            auto *window = input()->findToplevel(mouse->globalPosition());
            auto *effectWindow = window ? window->effectWindow() : nullptr;
            if (effectWindow && eligible(effectWindow)) {
                activate(effectWindow);
            } else {
                stop("no eligible window selected", true);
            }
        }
    }

    void grabbedKeyboardEvent(QKeyEvent *event) override
    {
        if (m_selecting && event->type() == QEvent::KeyRelease && event->key() == Qt::Key_Escape) {
            stop("selection cancelled", true);
        }
    }

public Q_SLOTS:
    Q_SCRIPTABLE bool Start()
    {
        if (!calledFromDBus() || !m_owner.watchedServices().isEmpty()
            || effects->isScreenLocked() || effects->activeFullScreenEffect()
            || effects->isMouseInterception() || input()->isSelectingWindow()
            || !effects->grabKeyboard(this)) {
            return false;
        }
        m_owner.addWatchedService(message().service());
        m_selecting = true;
        effects->startMouseInterception(this, Qt::CrossCursor);
        qInfo("picalens selecting an existing window");
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

private:
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

    void start(EffectWindow *window)
    {
        m_pendingActivation.clear();
        m_activationTimer.stop();
        if (!eligible(window)) {
            stop("selected window is no longer eligible");
            return;
        }
        auto *source = window->window();
        auto presentation = PicaLens::presentationFor(window);
        if (!source->surface() || !presentation) {
            stop("window has no usable presentation");
            return;
        }

        m_window = window;
        m_source = source;
        m_presentation = presentation;
        source->refOffscreenRendering();
        connect(window, &EffectWindow::windowDamaged, this, [this] {
            effects->addRepaint(m_presentation->outputRect);
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
        effects->addRepaint(presentation->outputRect);
        qInfo().nospace() << "picalens started protocol=" << (window->isX11Client() ? "xwayland" : "wayland")
                          << " source=" << presentation->source << " destination="
                          << presentation->destination << " output=" << presentation->outputRect;
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
        m_firstPaint = false;
        m_failedPaint = false;
        m_input.clear();
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
        effects->addRepaint(repaint);
    }

    QPointer<EffectWindow> m_window;
    QPointer<EffectWindow> m_pendingActivation;
    QPointer<Window> m_source;
    QTimer m_activationTimer;
    QDBusServiceWatcher m_owner;
    std::optional<PicaLens::Presentation> m_presentation;
    PicaLensInputFilter m_input;
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
