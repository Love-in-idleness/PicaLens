// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <QRectF>

#include <optional>

class QRegion;

namespace KWin
{
class EffectWindow;
class Output;
class RenderTarget;
class RenderViewport;
}

namespace PicaLens
{

struct Presentation {
    KWin::Output *output;
    QRect outputRect;
    QRectF source;
    QRect destination;
};

std::optional<Presentation> presentationFor(KWin::EffectWindow *window);
bool paintPresentation(KWin::EffectWindow *window, const Presentation &presentation,
                       const KWin::RenderTarget &target, const KWin::RenderViewport &viewport,
                       const QRegion &damage);

}
