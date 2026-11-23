// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <QRectF>

#include <optional>

class QRegion;

namespace KWin
{
class EffectWindow;
class Output;
class LogicalOutput;
class Region;
class RenderTarget;
class RenderViewport;
}

namespace PicaLens
{

#ifdef PICALENS_KWIN_67
using Screen = KWin::LogicalOutput;
using Damage = KWin::Region;
#else
using Screen = KWin::Output;
using Damage = QRegion;
#endif

struct Presentation {
    Screen *output;
    QRect outputRect;
    QRectF source;
    QRect destination;
};

QRect paintRect(const KWin::RenderViewport &viewport, const QRect &logicalRect);
std::optional<Presentation> presentationFor(KWin::EffectWindow *window);
bool paintPresentation(KWin::EffectWindow *window, const Presentation &presentation,
                       const KWin::RenderTarget &target, const KWin::RenderViewport &viewport,
                       const Damage &damage);

}
