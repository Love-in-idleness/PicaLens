// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <QImage>
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

enum class Layout { Fit, Stretch, Fill };

struct Crop {
    int left = 0;
    int top = 0;
    int right = 0;
    int bottom = 0;

    bool isEmpty() const
    {
        return left == 0 && top == 0 && right == 0 && bottom == 0;
    }
};

QRect paintRect(const KWin::RenderViewport &viewport, const QRect &logicalRect);
void clearGlDamage(const KWin::RenderTarget &target, const KWin::RenderViewport &viewport,
                   const Damage &damage);
std::optional<Presentation> presentationFor(KWin::EffectWindow *window, Layout layout, Crop crop = {});
bool paintPresentation(KWin::EffectWindow *window, const Presentation &presentation,
                       const KWin::RenderTarget &target, const KWin::RenderViewport &viewport,
                       const Damage &damage);
QImage captureDestination(const Presentation &presentation, const KWin::RenderTarget &target,
                          const KWin::RenderViewport &viewport);

}
