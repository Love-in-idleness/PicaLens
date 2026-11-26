// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include "presentation.h"

#include <memory>

namespace PicaLens
{

enum class Filter { Linear, Fsr, Sgsr };

class GpuPresentation
{
public:
    explicit GpuPresentation(Filter filter);
    ~GpuPresentation();
    bool paint(KWin::EffectWindow *window, const Presentation &presentation,
               const KWin::RenderTarget &target, const KWin::RenderViewport &viewport,
               const Damage &damage);

private:
    struct Pipeline;
    std::unique_ptr<Pipeline> m_pipeline;
};

}
