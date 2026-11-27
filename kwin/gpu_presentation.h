// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include "presentation.h"

#include <QString>
#include <functional>
#include <memory>

namespace PicaLens
{

enum class Filter { Linear, Fsr, Sgsr, Anime4k };

class GpuPresentation
{
public:
    GpuPresentation(Filter filter, const QString &device, std::function<void()> repaint);
    ~GpuPresentation();
    static bool workerAvailable();
    bool paint(KWin::EffectWindow *window, const Presentation &presentation,
               const KWin::RenderTarget &target, const KWin::RenderViewport &viewport,
               const Damage &damage);
    void sourceDamaged();
    bool pending() const;
    bool hasFrame() const;
    const char *error() const;

private:
    struct Pipeline;
    std::unique_ptr<Pipeline> m_pipeline;
};

}
