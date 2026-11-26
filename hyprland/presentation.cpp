#include "gpu.h"

#include <algorithm>
#include <any>
#include <array>
#include <cerrno>
#include <cstddef>
#include <cstring>
#include <memory>
#include <optional>
#include <string_view>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#include <stdexcept>
#include <unordered_map>
#include <unordered_set>
#include <vector>

#include <hyprland/src/Compositor.hpp>
#include <hyprland/src/desktop/DesktopTypes.hpp>
#include <hyprland/src/desktop/state/FocusState.hpp>
#include <hyprland/src/desktop/view/WLSurface.hpp>
#include <hyprland/src/desktop/view/Window.hpp>
#include <hyprland/src/devices/IKeyboard.hpp>
#include <hyprland/src/devices/IPointer.hpp>
#include <hyprland/src/managers/SeatManager.hpp>
#include <hyprland/src/managers/SessionLockManager.hpp>
#include <hyprland/src/managers/input/InputManager.hpp>
#include <hyprland/src/plugins/PluginAPI.hpp>
#include <hyprland/src/render/Renderer.hpp>
#include <hyprland/src/render/pass/RectPassElement.hpp>
#include <hyprland/src/render/pass/RendererHintsPassElement.hpp>

namespace {

using WindowRenderer = void (*)(CHyprRenderer*, PHLWINDOW, PHLMONITOR,
    const Time::steady_tp&, bool, eRenderPassMode, bool, bool);

WindowRenderer renderWindow = nullptr;
PHLWINDOWREF target;
std::vector<SP<HOOK_CALLBACK_FN>> callbacks;
std::unordered_set<uint32_t> presses;
std::unordered_set<uint32_t> ownedReleases;
SP<CWLSurfaceResource> grabbed;
std::optional<bool> oldScanoutBlock;
bool escapePressed = false;

std::unique_ptr<PicaLens::Upscaler> upscaler;
bool filterReady = false;

enum class ControlPhase { Reading, Selecting, Presenting };

struct Control {
    int listener = -1;
    wl_event_source* listenerEvent = nullptr;
    int client = -1;
    wl_event_source* clientEvent = nullptr;
    std::string line;
    ControlPhase phase = ControlPhase::Reading;
    PicaLens::Filter filter = PicaLens::Filter::Linear;
};

Control control;

void disconnect() {
    if (control.clientEvent)
        wl_event_source_remove(control.clientEvent);
    if (control.client >= 0)
        close(control.client);
    control.clientEvent = nullptr;
    control.client = -1;
    control.line.clear();
    control.phase = ControlPhase::Reading;
    control.filter = PicaLens::Filter::Linear;
}

bool reply(std::string_view message) {
    return control.client >= 0
        && send(control.client, message.data(), message.size(), MSG_NOSIGNAL) == static_cast<ssize_t>(message.size());
}

class PresentationBackground : public CRectPassElement {
  public:
    using CRectPassElement::CRectPassElement;
    // The pass optimizer uses untransformed window boxes to calculate occlusion.
    bool disableSimplification() override { return true; }
};

struct Geometry {
    CBox source;
    CBox destination;
    double zoom;
};

Geometry geometry(PHLWINDOW window, PHLMONITOR monitor) {
    const auto source = window->getWindowMainSurfaceBox();
    const auto zoom = std::min(monitor->m_size.x / source.w, monitor->m_size.y / source.h);
    const auto size = source.size() * zoom;
    const auto position = monitor->m_position + (monitor->m_size - size) / 2.;
    return {source, {position, size}, zoom};
}

void stop() {
    const auto window = target.lock();
    const bool wasVisible = control.phase != ControlPhase::Reading;
    target.reset();
    upscaler.reset();
    filterReady = false;
    if (oldScanoutBlock.has_value()) {
        g_pHyprRenderer->m_directScanoutBlocked = *oldScanoutBlock;
        oldScanoutBlock.reset();
    }
    if (control.phase == ControlPhase::Selecting)
        g_pHyprRenderer->setCursorFromName("left_ptr", true);
    disconnect();
    if (!wasVisible)
        return;
    const auto now = Time::millis(Time::steadyNow());
    if (grabbed && grabbed == g_pSeatManager->m_state.pointerFocus) {
        for (const auto code : presses)
            g_pSeatManager->sendPointerButton(now, code, WL_POINTER_BUTTON_STATE_RELEASED);
        g_pSeatManager->sendPointerFrame();
    }
    presses.clear();
    grabbed.reset();
    if (window && window->m_monitor)
        g_pHyprRenderer->damageMonitor(window->m_monitor.lock());
    g_pInputManager->simulateMouseMovement();
}

PHLWINDOW activeTarget() {
    const auto window = target.lock();
    if (!window) {
        if (control.phase == ControlPhase::Presenting)
            stop();
        return nullptr;
    }
    if (!window->m_isMapped || window->m_fadingOut || window->isHidden() || !window->m_monitor
        || Desktop::focusState()->window() != window || g_pSessionLockManager->isSessionLocked()
        || g_pInputManager->isConstrained() || (upscaler && window->popupsCount() > 0)) {
        stop();
        return nullptr;
    }
    return window;
}

bool routePointer(PHLWINDOW window, uint32_t time, bool motion) {
    if (upscaler && !filterReady)
        return false;
    const auto monitor = window->m_monitor.lock();
    const auto layout = geometry(window, monitor);
    const auto cursor = g_pInputManager->getMouseCoordsInternal();
    const auto position = layout.source.pos() + (cursor - layout.destination.pos()) / layout.zoom;
    Vector2D local;
    SP<CWLSurfaceResource> surface;
    if (grabbed) {
        surface = grabbed;
        local = g_pCompositor->vectorToSurfaceLocal(position, window, surface);
        if (window->m_isX11)
            local *= window->m_X11SurfaceScaledBy;
    } else if (layout.destination.containsPoint(cursor)) {
        if (window->m_isX11) {
            surface = window->wlSurface()->resource();
            local = (position - layout.source.pos()) * window->m_X11SurfaceScaledBy;
        } else {
            surface = g_pCompositor->vectorWindowToSurface(position, window, local);
        }
    }
    g_pSeatManager->setPointerFocus(surface, local);
    if (!surface || g_pSeatManager->m_state.pointerFocus != surface)
        return false;
    if (motion)
        g_pSeatManager->sendPointerMotion(time, local);
    return true;
}

class CapturePass : public IPassElement {
  public:
    bool needsLiveBlur() override { return false; }
    bool needsPrecomputeBlur() override { return false; }
    bool disableSimplification() override { return true; }
    const char* passName() override { return "PicaLens source capture"; }
    void draw(const CRegion&) override {
        if (upscaler)
            upscaler->begin();
    }
};

class UpscalePass : public IPassElement {
  public:
    bool needsLiveBlur() override { return false; }
    bool needsPrecomputeBlur() override { return false; }
    const char* passName() override { return "PicaLens upscaling"; }
    void draw(const CRegion& damage) override {
        if (!upscaler)
            return;
        if (!upscaler->finish(damage)) {
            reply("{\"Err\":\"interactive_filter_unavailable\"}\n");
            stop();
            return;
        }
        if (!filterReady) {
            Log::logger->log(Log::INFO, "PicaLens interactive filter {}",
                control.filter == PicaLens::Filter::Fsr ? "fsr" : "sgsr");
            filterReady = true;
        }
    }
};

void paint(void*, SCallbackInfo&, std::any payload) {
    if (std::any_cast<eRenderStage>(payload) != RENDER_POST_WINDOWS)
        return;
    const auto window = activeTarget();
    const auto monitor = g_pHyprOpenGL->m_renderData.pMonitor.lock();
    if (!window || !monitor || window->m_monitor != monitor)
        return;
    const auto layout = geometry(window, monitor);
    auto& pass = g_pHyprRenderer->m_renderPass;
    SRenderModifData transform;
    if (upscaler) {
        const auto topLevel = window->wlSurface();
        const auto resource = topLevel ? topLevel->resource() : nullptr;
        if (!resource || monitor->m_transform != WL_OUTPUT_TRANSFORM_NORMAL
            || resource->m_current.size.x <= 0 || resource->m_current.size.y <= 0
            || !window->m_transformers.empty()) {
            reply("{\"Err\":\"interactive_filter_unavailable\"}\n");
            stop();
            return;
        }
        const auto& surface = resource->m_current;
        auto pixels = surface.bufferSize;
        if (surface.viewport.hasSource)
            pixels = surface.viewport.source.size() * surface.scale;
        else if (surface.transform % 2)
            std::swap(pixels.x, pixels.y);
        const auto density = std::min(pixels.x / surface.size.x, pixels.y / surface.size.y);
        const auto source = (layout.source.size() * density).round();
        auto destination = layout.destination;
        destination.translate(-monitor->m_position).scale(monitor->m_scale).round();
        if (!upscaler->prepare(monitor->m_pixelSize, source, destination)) {
            reply("{\"Err\":\"interactive_filter_unavailable\"}\n");
            stop();
            return;
        }
        pass.add(makeUnique<CapturePass>());
        transform.modifs.emplace_back(SRenderModifData::RMOD_TYPE_SCALE,
            static_cast<float>(density / monitor->m_scale));
        transform.modifs.emplace_back(SRenderModifData::RMOD_TYPE_TRANSLATE,
            (monitor->m_position - layout.source.pos()) * density);
    } else {
        CRectPassElement::SRectData background;
        background.box = {{0, 0}, monitor->m_transformedSize};
        background.color = CHyprColor{0., 0., 0., 1.};
        pass.add(makeUnique<PresentationBackground>(background));
        transform.modifs.emplace_back(SRenderModifData::RMOD_TYPE_SCALE, static_cast<float>(layout.zoom));
        const auto offset = (layout.destination.pos() - monitor->m_position
            - (layout.source.pos() - monitor->m_position) * layout.zoom) * monitor->m_scale;
        transform.modifs.emplace_back(SRenderModifData::RMOD_TYPE_TRANSLATE, offset);
    }
    pass.add(makeUnique<CRendererHintsPassElement>(CRendererHintsPassElement::SData{transform}));
    renderWindow(g_pHyprRenderer.get(), window, monitor, Time::steadyNow(), false,
        upscaler ? RENDER_PASS_MAIN : RENDER_PASS_ALL, false, true);
    pass.add(makeUnique<CRendererHintsPassElement>(CRendererHintsPassElement::SData{SRenderModifData{}}));
    if (upscaler)
        pass.add(makeUnique<UpscalePass>());
}

void motion(void*, SCallbackInfo& info, std::any) {
    if (const auto window = activeTarget()) {
        info.cancelled = true;
        routePointer(window, Time::millis(Time::steadyNow()), true);
        g_pSeatManager->sendPointerFrame();
    }
}

bool begin(PHLWINDOW window) {
    if (!window || !window->m_isMapped || window->isHidden() || !window->m_monitor
        || g_pSessionLockManager->isSessionLocked() || g_pInputManager->isConstrained())
        return false;
    const auto box = window->getWindowMainSurfaceBox();
    if (box.w <= 0 || box.h <= 0)
        return false;
    oldScanoutBlock = g_pHyprRenderer->m_directScanoutBlocked;
    g_pHyprRenderer->m_directScanoutBlocked = true;
    target = window;
    if (control.filter != PicaLens::Filter::Linear)
        upscaler = std::make_unique<PicaLens::Upscaler>(control.filter);
    control.phase = ControlPhase::Presenting;
    Desktop::focusState()->rawWindowFocus(window);
    if (target.expired())
        return false;
    g_pHyprRenderer->damageMonitor(window->m_monitor.lock());
    routePointer(window, Time::millis(Time::steadyNow()), true);
    g_pSeatManager->sendPointerFrame();
    return true;
}

void cancelPicker() {
    reply("{\"Ok\":\"PicaLensCancelled\"}\n");
    stop();
}

void click(void*, SCallbackInfo& info, std::any payload) {
    const auto event = std::any_cast<IPointer::SButtonEvent>(payload);
    const auto window = activeTarget();
    const bool pressed = event.state == WL_POINTER_BUTTON_STATE_PRESSED;
    if (pressed && control.phase == ControlPhase::Selecting) {
        info.cancelled = true;
        ownedReleases.insert(event.button);
        if (event.button != BTN_LEFT) {
            cancelPicker();
            return;
        }
        const auto source = g_pCompositor->vectorToWindowUnified(
            g_pInputManager->getMouseCoordsInternal(), Desktop::View::ALLOW_FLOATING | Desktop::View::INPUT_EXTENTS);
        if (!begin(source)) {
            cancelPicker();
            return;
        }
        if (!reply("{\"Ok\":\"Handled\"}\n"))
            stop();
        return;
    }
    // Swallow releases for presses held by the picker or the magnified window.
    if (!pressed && ownedReleases.erase(event.button)) {
        info.cancelled = true;
        if (window && presses.erase(event.button)) {
            g_pSeatManager->sendPointerButton(event.timeMs, event.button, event.state);
            g_pSeatManager->sendPointerFrame();
            if (presses.empty())
                grabbed.reset();
        }
        return;
    }
    if (!window)
        return;
    info.cancelled = true;
    if (pressed) {
        ownedReleases.insert(event.button);
        if (routePointer(window, event.timeMs, true)) {
            grabbed = g_pSeatManager->m_state.pointerFocus.lock();
            presses.insert(event.button);
            g_pSeatManager->sendPointerButton(event.timeMs, event.button, event.state);
        }
        g_pSeatManager->sendPointerFrame();
    }
}

void wheel(void*, SCallbackInfo& info, std::any payload) {
    const auto window = activeTarget();
    if (!window)
        return;
    info.cancelled = true;
    const auto& map = std::any_cast<const std::unordered_map<std::string, std::any>&>(payload);
    const auto event = std::any_cast<IPointer::SAxisEvent>(map.at("event"));
    if (routePointer(window, event.timeMs, false))
        g_pSeatManager->sendPointerAxis(event.timeMs, event.axis, event.delta,
            event.deltaDiscrete / 120, event.deltaDiscrete, event.source, event.relativeDirection);
    g_pSeatManager->sendPointerFrame();
}

void key(void*, SCallbackInfo& info, std::any payload) {
    const auto& map = std::any_cast<const std::unordered_map<std::string, std::any>&>(payload);
    const auto keyboard = std::any_cast<SP<IKeyboard>>(map.at("keyboard"));
    const auto event = std::any_cast<IKeyboard::SKeyEvent>(map.at("event"));
    if (!keyboard->m_xkbState || xkb_state_key_get_one_sym(keyboard->m_xkbState, event.keycode + 8) != XKB_KEY_Escape)
        return;
    if (event.state == WL_KEYBOARD_KEY_STATE_RELEASED && escapePressed) {
        escapePressed = false;
        info.cancelled = true;
        return;
    }
    if (event.state != WL_KEYBOARD_KEY_STATE_PRESSED || control.phase == ControlPhase::Reading)
        return;
    escapePressed = true;
    info.cancelled = true;
    if (control.phase == ControlPhase::Selecting)
        cancelPicker();
    else
        stop();
}

int onClient(int fd, uint32_t events, void*) {
    if (events & (WL_EVENT_ERROR | WL_EVENT_HANGUP)) {
        stop();
        return 0;
    }
    std::array<char, 128> buffer;
    const auto length = recv(fd, buffer.data(), buffer.size(), 0);
    if (length < 0 && (errno == EAGAIN || errno == EINTR))
        return 0;
    if (length <= 0 || control.phase != ControlPhase::Reading
        || control.line.size() + static_cast<size_t>(length) > buffer.size()) {
        stop();
        return 0;
    }
    control.line.append(buffer.data(), static_cast<size_t>(length));
    if (control.line.back() != '\n')
        return 0;
    if (control.line == "\"PicaLensVersion\"\n") {
        control.line.clear();
        if (!reply("{\"Ok\":{\"PicaLensVersion\":2}}\n"))
            stop();
    } else {
        if (control.line == "{\"PicaLensStart\":{\"id\":null,\"filter\":\"fsr\"}}\n")
            control.filter = PicaLens::Filter::Fsr;
        else if (control.line == "{\"PicaLensStart\":{\"id\":null,\"filter\":\"sgsr\"}}\n")
            control.filter = PicaLens::Filter::Sgsr;
        else if (control.line != "{\"PicaLensStart\":{\"id\":null}}\n") {
            stop();
            return 0;
        }
        control.line.clear();
        if (g_pSessionLockManager->isSessionLocked() || g_pInputManager->isConstrained()) {
            reply("{\"Err\":\"picalens_busy\"}\n");
            stop();
        } else {
            control.phase = ControlPhase::Selecting;
            g_pHyprRenderer->setCursorFromName("crosshair", true);
        }
    }
    return 0;
}

int acceptClient(int fd, uint32_t, void*) {
    const int client = accept4(fd, nullptr, nullptr, SOCK_NONBLOCK | SOCK_CLOEXEC);
    if (client < 0)
        return 0;
    ucred peer{};
    socklen_t size = sizeof(peer);
    if (control.client >= 0 || getsockopt(client, SOL_SOCKET, SO_PEERCRED, &peer, &size) != 0 || peer.uid != geteuid()) {
        close(client);
        return 0;
    }
    control.client = client;
    control.clientEvent = wl_event_loop_add_fd(g_pCompositor->m_wlEventLoop, client, WL_EVENT_READABLE, onClient, nullptr);
    if (!control.clientEvent)
        disconnect();
    return 0;
}

void listenForPicaLens() {
    const auto name = "picalens-hypr-" + std::to_string(geteuid()) + "-" + g_pCompositor->m_instanceSignature;
    sockaddr_un address{};
    if (name.size() >= sizeof(address.sun_path))
        throw std::runtime_error("Hyprland PicaLens socket path is too long");
    address.sun_family = AF_UNIX;
    // Abstract address, scoped to this compositor instance and checked against peer credentials.
    std::memcpy(address.sun_path + 1, name.data(), name.size());
    const auto length = offsetof(sockaddr_un, sun_path) + 1 + name.size();
    control.listener = socket(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK | SOCK_CLOEXEC, 0);
    if (control.listener < 0)
        throw std::runtime_error("Could not create the PicaLens socket");
    if (bind(control.listener, reinterpret_cast<sockaddr*>(&address), length) == 0
        && listen(control.listener, 1) == 0) {
        control.listenerEvent = wl_event_loop_add_fd(
            g_pCompositor->m_wlEventLoop, control.listener, WL_EVENT_READABLE, acceptClient, nullptr);
    }
    if (!control.listenerEvent) {
        close(control.listener);
        control.listener = -1;
        throw std::runtime_error("Could not start the PicaLens socket");
    }
}

} // namespace

APICALL EXPORT std::string PLUGIN_API_VERSION() {
    return HYPRLAND_API_VERSION;
}

APICALL EXPORT PLUGIN_DESCRIPTION_INFO PLUGIN_INIT(HANDLE handle) {
    if (std::strcmp(__hyprland_api_get_hash(), __hyprland_api_get_client_hash()) != 0)
        throw std::runtime_error("Hyprland headers do not match the running compositor");
    for (const auto& function : HyprlandAPI::findFunctionsByName(handle, "renderWindow")) {
        if (function.demangled.starts_with("CHyprRenderer::renderWindow(")) {
            renderWindow = reinterpret_cast<WindowRenderer>(function.address);
            break;
        }
    }
    if (!renderWindow)
        throw std::runtime_error("Hyprland window rendering entry point is unavailable");
    callbacks.push_back(HyprlandAPI::registerCallbackDynamic(handle, "render", paint));
    callbacks.push_back(HyprlandAPI::registerCallbackDynamic(handle, "preRender", [](void*, SCallbackInfo&, std::any data) {
        const auto monitor = std::any_cast<PHLMONITOR>(data);
        if (const auto window = activeTarget(); window && window->m_monitor == monitor)
            monitor->m_damage.damageEntire();
    }));
    callbacks.push_back(HyprlandAPI::registerCallbackDynamic(handle, "activeWindow", [](void*, SCallbackInfo&, std::any) {
        activeTarget();
    }));
    callbacks.push_back(HyprlandAPI::registerCallbackDynamic(handle, "mouseMove", motion));
    callbacks.push_back(HyprlandAPI::registerCallbackDynamic(handle, "mouseButton", click));
    callbacks.push_back(HyprlandAPI::registerCallbackDynamic(handle, "mouseAxis", wheel));
    callbacks.push_back(HyprlandAPI::registerCallbackDynamic(handle, "keyPress", key));
    listenForPicaLens();
    return {"picalens", "Interactive original-window magnification", "PicaLens", "0.1"};
}

APICALL EXPORT void PLUGIN_EXIT() {
    stop();
    callbacks.clear();
    if (control.listenerEvent)
        wl_event_source_remove(control.listenerEvent);
    if (control.listener >= 0)
        close(control.listener);
}
