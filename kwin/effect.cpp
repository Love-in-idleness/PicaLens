// SPDX-License-Identifier: GPL-2.0-or-later

#include "inputfilter.h"
#include "presentation.h"

#include <KGlobalAccel>
#include <QAction>
#include <QDebug>
#include <QPointer>
#include <QTimer>

#include "effect/effect.h"
#include "effect/effecthandler.h"
#include "effect/effectwindow.h"
#include "wayland_server.h"
#include "window.h"

namespace KWin
{

class PicaLensEffect : public Effect
{
public:
    PicaLensEffect()
    {
        auto *toggleAction = new QAction(this);
        toggleAction->setProperty("componentName", QStringLiteral("kwin"));
        toggleAction->setObjectName(QStringLiteral("PicaLensToggle"));
        toggleAction->setText(tr("Toggle PicaLens"));
        const QList<QKeySequence> shortcuts = {QKeySequence(Qt::CTRL | Qt::META | Qt::Key_Y)};
        KGlobalAccel::self()->setDefaultShortcut(toggleAction, shortcuts);
        KGlobalAccel::self()->setShortcut(toggleAction, shortcuts);
        connect(toggleAction, &QAction::triggered, this, &PicaLensEffect::toggle);
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
            if (m_presentation && effects->activeFullScreenEffect() != this) {
                stop("another fullscreen effect took over");
            }
        });
        qInfo("picalens loaded");
    }

    ~PicaLensEffect() override
    {
        stop("effect unloaded");
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

private:
    void toggle()
    {
        if (m_presentation || m_pendingActivation) {
            stop("shortcut");
            return;
        }
        if (m_selecting || effects->isScreenLocked() || effects->activeFullScreenEffect()) {
            return;
        }
        m_selecting = true;
        qInfo("picalens selecting an existing window");
        effects->startInteractiveWindowSelection([effect = QPointer(this)](EffectWindow *window) {
            if (!effect || !effect->m_selecting) {
                return;
            }
            effect->m_selecting = false;
            if (window) {
                effect->activate(window);
            } else {
                qInfo("picalens selection cancelled");
            }
        });
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
        if (!eligible(window)) {
            qInfo("picalens rejected ineligible window");
            return;
        }
        m_pendingActivation = window;
        m_activationTimer.start(1500);
        // KWin calls the selection callback before releasing its keyboard grab.
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
            qInfo("picalens rejected ineligible window");
            return;
        }
        auto *source = window->window();
        auto presentation = PicaLens::presentationFor(window);
        if (!source->surface() || !presentation) {
            qInfo("picalens rejected window without a usable presentation");
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

    void stop(const char *reason)
    {
        m_selecting = false;
        m_activationTimer.stop();
        if (m_pendingActivation) {
            m_pendingActivation.clear();
            qInfo("picalens activation cancelled: %s", reason);
        }
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
        qInfo("picalens stopped: %s", reason);
    }

    QPointer<EffectWindow> m_window;
    QPointer<EffectWindow> m_pendingActivation;
    QPointer<Window> m_source;
    QTimer m_activationTimer;
    std::optional<PicaLens::Presentation> m_presentation;
    PicaLensInputFilter m_input;
    bool m_selecting = false;
    bool m_firstPaint = false;
    bool m_failedPaint = false;
};

KWIN_EFFECT_FACTORY_SUPPORTED_ENABLED(PicaLensEffect, "metadata.json",
                                      return waylandServer() != nullptr;,
                                      return false;)

} // namespace KWin

#include "effect.moc"
