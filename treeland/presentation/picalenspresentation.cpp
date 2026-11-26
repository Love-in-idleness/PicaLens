// SPDX-License-Identifier: Apache-2.0

#include "picalenspresentation.h"

#include "core/waylandserver.h"
#include "output/output.h"
#include "session/session.h"
#include "session/sessionmanager.h"
#include "surface/surfacewrapper.h"

#include <woutputitem.h>
#include <wsocket.h>
#include <woutputrenderwindow.h>
#include <wsurface.h>
#include <wsurfaceitem.h>

#include <QJsonDocument>
#include <QJsonObject>
#include <QKeyEvent>
#include <QLocalServer>
#include <QLocalSocket>
#include <QMatrix4x4>
#include <QMouseEvent>
#include <QOpenGLContext>
#include <QQmlComponent>
#include <QQuickItem>
#include <QTimer>
#include <QWheelEvent>
#include <sys/socket.h>

#include <algorithm>
#include <cmath>
#include <utility>

namespace {

constexpr qint64 maxRequestBytes = 256;

class WindowTransform : public QQuickTransform
{
public:
    WindowTransform(QQuickItem *item, QPointF offset, qreal factor)
        : QQuickTransform(item), m_offset(offset), m_factor(factor)
    {
        appendToItem(item);
    }

    void applyTo(QMatrix4x4 *matrix) const override
    {
        matrix->translate(m_offset.x(), m_offset.y());
        matrix->scale(m_factor, m_factor);
    }

private:
    QPointF m_offset;
    qreal m_factor;
};

bool suitable(WaylandServer *server, SurfaceWrapper *surface)
{
    return surface && surface->isVisible() && surface->ownsOutput()
        && surface->surfaceItem() && surface->surfaceItem()->surface()
        && (surface->type() == SurfaceWrapper::Type::XdgToplevel
            || surface->type() == SurfaceWrapper::Type::XWayland)
        && server->surfaceBelongsToCurrentSession(surface);
}

}

PicaLensPresentation::PicaLensPresentation(WaylandServer *server)
    : QObject(server)
    , m_server(server)
    , m_listener(new QLocalServer(this))
    , m_deadline(new QTimer(this))
{
    m_listener->setSocketOptions(QLocalServer::AbstractNamespaceOption);
    m_deadline->setSingleShot(true);
    connect(m_deadline, &QTimer::timeout, this, &PicaLensPresentation::fail);
    connect(m_listener, &QLocalServer::newConnection, this, &PicaLensPresentation::accept);
    connect(server->sessionManager(), &SessionManager::socketFileChanged,
            this, &PicaLensPresentation::listen);
    connect(server->sessionManager(), &SessionManager::sessionChanged,
            this, &PicaLensPresentation::listen);
    connect(server, &WaylandServer::currentModeChanged, this, &PicaLensPresentation::stop);
    connect(server, &WaylandServer::activatedSurfaceChanged, this, [this] {
        if (m_source && m_server->activatedSurface() != m_source)
            stop();
    });
    server->window()->installEventFilter(this);
    listen();
}

PicaLensPresentation::~PicaLensPresentation()
{
    stop();
}

void PicaLensPresentation::listen()
{
    const auto *session = m_server->activeSession();
    if (!session || !session->socket() || !session->socket()->isValid()) {
        stop();
        m_listener->close();
        return;
    }
    const auto name = QStringLiteral("picalens-treeland-%1").arg(session->uid());
    if (m_listener->isListening() && m_listener->serverName() == name)
        return;
    stop();
    m_listener->close();
    m_uid = session->uid();
    m_listener->listen(name);
}

void PicaLensPresentation::accept()
{
    auto *connection = m_listener->nextPendingConnection();
    if (!connection)
        return;
    ucred peer{};
    socklen_t length = sizeof(peer);
    if (m_client || getsockopt(connection->socketDescriptor(), SOL_SOCKET, SO_PEERCRED,
                               &peer, &length) != 0 || peer.uid != m_uid) {
        connection->disconnectFromServer();
        connection->deleteLater();
        return;
    }
    m_client = connection;
    connection->setReadBufferSize(maxRequestBytes + 1);
    connect(connection, &QLocalSocket::readyRead, this, &PicaLensPresentation::readRequest);
    connect(connection, &QLocalSocket::disconnected, this, &PicaLensPresentation::stop);
    m_deadline->start(3000);
}

void PicaLensPresentation::readRequest()
{
    if (m_client->bytesAvailable() > maxRequestBytes || m_phase != Phase::Idle) {
        stop();
        return;
    }
    if (!m_client->canReadLine())
        return;
    const auto line = m_client->readLine(maxRequestBytes + 1);
    if (line == "\"PicaLensVersion\"\n") {
        m_client->write("{\"Ok\":{\"PicaLensVersion\":2}}\n");
        return;
    }
    const auto request = QJsonDocument::fromJson(line).object()
        .value(QStringLiteral("PicaLensStart")).toObject();
    m_filter = request.value(QStringLiteral("filter")).toString(QStringLiteral("linear"));
    if (request.isEmpty() || !request.value(QStringLiteral("id")).isNull()
        || (m_filter != QStringLiteral("linear") && m_filter != QStringLiteral("fsr")
            && m_filter != QStringLiteral("sgsr"))) {
        stop();
        return;
    }
    if (m_server->currentMode() != WaylandServer::CurrentMode::Normal) {
        m_client->write("{\"Err\":\"picalens_busy\"}\n");
        stop();
        return;
    }
    m_deadline->stop();
    m_phase = Phase::Selecting;
}

SurfaceWrapper *PicaLensPresentation::pick(const QPointF &position) const
{
    const auto items = WOutputRenderWindow::paintOrderItemList(
        m_server->window()->contentItem(), [this](QQuickItem *item) {
            return suitable(m_server, qobject_cast<SurfaceWrapper *>(item));
        });
    for (auto item = items.crbegin(); item != items.crend(); ++item) {
        if (*item && (*item)->contains((*item)->mapFromScene(position)))
            return qobject_cast<SurfaceWrapper *>(item->data());
    }
    return nullptr;
}

bool PicaLensPresentation::eventFilter(QObject *, QEvent *event)
{
    if (m_phase == Phase::Idle)
        return false;

    if (event->type() == QEvent::KeyPress
        && static_cast<QKeyEvent *>(event)->key() == Qt::Key_Escape) {
        if (m_phase == Phase::Selecting)
            m_client->write("{\"Ok\":\"PicaLensCancelled\"}\n");
        stop();
        return true;
    }

    if (m_phase == Phase::Selecting) {
        if (event->type() == QEvent::MouseButtonRelease) {
            const auto *mouse = static_cast<QMouseEvent *>(event);
            auto *source = mouse->button() == Qt::LeftButton ? pick(mouse->position()) : nullptr;
            if (!source) {
                m_client->write("{\"Ok\":\"PicaLensCancelled\"}\n");
                stop();
            } else if (!start(source)) {
                fail();
            }
            return true;
        }
        return event->type() == QEvent::MouseButtonPress
            || event->type() == QEvent::MouseButtonDblClick || event->type() == QEvent::Wheel;
    }

    if (event->type() == QEvent::Wheel) {
        const auto pos = static_cast<QWheelEvent *>(event)->position();
        return m_output.contains(pos) && (m_phase != Phase::Presenting || !m_picture.contains(pos));
    }
    if (event->type() == QEvent::MouseButtonPress || event->type() == QEvent::MouseButtonDblClick
        || event->type() == QEvent::MouseButtonRelease || event->type() == QEvent::MouseMove) {
        const auto *mouse = static_cast<QMouseEvent *>(event);
        const bool visible = m_phase == Phase::Presenting && m_picture.contains(mouse->position());
        if (event->type() == QEvent::MouseButtonRelease && m_pressed.testFlag(mouse->button())) {
            m_pressed.setFlag(mouse->button(), false);
            return false;
        }
        if (visible && (event->type() == QEvent::MouseButtonPress
                        || event->type() == QEvent::MouseButtonDblClick))
            m_pressed.setFlag(mouse->button());
        return m_output.contains(mouse->position()) && !visible && m_pressed == Qt::NoButton;
    }
    return false;
}

bool PicaLensPresentation::start(SurfaceWrapper *source)
{
    if (!suitable(m_server, source))
        return false;
    auto *output = source->ownsOutput();
    auto *root = m_server->window()->contentItem();
    m_output = output->outputItem()->mapRectToScene(output->outputItem()->boundingRect());
    // ShaderEffectSource captures the window item size, excluding decoration shadows.
    // Use the same rectangle to position the original item for pointer delivery.
    const QRectF sourceRect(QPointF(), source->size());
    const auto nativeBounds = source->mapRectToScene(sourceRect);
    if (m_output.isEmpty() || nativeBounds.isEmpty() || !source->parentItem())
        return false;
    m_zoom = std::min(m_output.width() / nativeBounds.width(),
                      m_output.height() / nativeBounds.height());
    m_picture = QRectF(QPointF(), nativeBounds.size() * m_zoom);
    m_picture.moveCenter(m_output.center());

    m_server->forceActivateSurface(source, Qt::MouseFocusReason, m_server->seat());
    if (m_server->activatedSurface() != source)
        return false;
    m_source = source;
    m_phase = Phase::Preparing;
    const auto retire = [this] { stop(); };
    m_watchers = {
        connect(m_server->window(), &QQuickWindow::afterRendering, this,
                &PicaLensPresentation::prepareInput, Qt::QueuedConnection),
        connect(source, &QObject::destroyed, this, retire),
        connect(source, &QQuickItem::visibleChanged, this, retire),
        connect(source, &QQuickItem::xChanged, this, retire),
        connect(source, &QQuickItem::yChanged, this, retire),
        connect(source, &QQuickItem::widthChanged, this, retire),
        connect(source, &QQuickItem::heightChanged, this, retire),
        connect(source, &SurfaceWrapper::ownsOutputChanged, this, retire),
        connect(output, &QObject::destroyed, this, retire),
        connect(output->output(), &WOutput::scaleChanged, this, retire),
        connect(output->output(), &WOutput::effectiveSizeChanged, this, retire),
        connect(output->outputItem(), &QQuickItem::xChanged, this, retire),
        connect(output->outputItem(), &QQuickItem::yChanged, this, retire),
    };

    if (m_filter != QStringLiteral("linear")) {
        auto *scene = m_server->window()->rendererInterface();
        if (scene->graphicsApi() != QSGRendererInterface::OpenGL)
            return false;
        const auto *context = static_cast<QOpenGLContext *>(scene->getResource(
            m_server->window(), QSGRendererInterface::OpenGLContextResource));
        if (!context || context->format().version() < (context->isOpenGLES()
                ? std::pair(3, 1) : std::pair(4, 3)))
            return false;
    }

    const auto *surface = source->surfaceItem()->surface();
    const auto buffer = surface->bufferSize();
    const auto logical = surface->size();
    if (buffer.isEmpty() || logical.isEmpty())
        return false;
    const auto density = std::min(qreal(buffer.width()) / logical.width(),
                                  qreal(buffer.height()) / logical.height());
    const QSize from = (source->size() * density).toSize();
    const QSizeF to = m_picture.size() * output->output()->scale();
    const double pixels = to.width() * to.height();
    const auto factor = std::min({1., from.width() * 2. / to.width(),
        from.height() * 2. / to.height(), std::sqrt(3840. * 2160. / pixels)});
    const QSize size(std::floor(to.width() * factor), std::floor(to.height() * factor));
    if (m_filter != QStringLiteral("linear")
        && (size.width() < from.width() || size.height() < from.height() || size == from))
        return false;

    QQmlComponent component(m_server->qmlEngine(), QUrl(QStringLiteral("qrc:/picalens/Presentation.qml")));
    auto *item = qobject_cast<QQuickItem *>(component.createWithInitialProperties({
        {QStringLiteral("sourceWindow"), QVariant::fromValue(source)},
        {QStringLiteral("sourceRect"), sourceRect},
        {QStringLiteral("filter"), m_filter},
        {QStringLiteral("sourcePixels"), from},
        {QStringLiteral("filterPixels"), size},
        {QStringLiteral("picture"), m_picture.translated(-m_output.topLeft())},
    }));
    if (!item)
        return false;
    m_view = item;
    item->setParent(this);
    item->setParentItem(root);
    item->setPosition(m_output.topLeft());
    item->setSize(m_output.size());
    item->setZ(10000);
    m_deadline->start(3000);
    m_server->window()->update();
    return true;
}

void PicaLensPresentation::prepareInput()
{
    if (m_phase != Phase::Preparing || !m_source)
        return;
    auto *parent = m_source->parentItem();
    if (!parent || !m_view || m_view->property("failed").toBool()) {
        fail();
        return;
    }
    if (!m_view->property("ready").toBool()) {
        m_server->window()->update();
        return;
    }
    const auto offset = parent->mapFromScene(m_picture.topLeft())
        - m_source->mapToItem(parent, QPointF());
    m_transform = new WindowTransform(m_source, offset, m_zoom);
    if (m_view)
        m_view->setProperty("captureHidden", true);
    m_deadline->stop();
    m_phase = Phase::Presenting;
    m_client->write("{\"Ok\":\"Handled\"}\n");
}

void PicaLensPresentation::fail()
{
    if (m_client)
        m_client->write("{\"Err\":\"interactive_filter_unavailable\"}\n");
    stop();
}

void PicaLensPresentation::stop()
{
    m_phase = Phase::Idle;
    m_pressed = Qt::NoButton;
    m_deadline->stop();
    for (auto connection : std::exchange(m_watchers, {}))
        disconnect(connection);
    delete m_view.data();
    delete m_transform.data();
    m_source.clear();
    if (auto *connection = m_client.data()) {
        m_client.clear();
        disconnect(connection, nullptr, this, nullptr);
        connection->flush();
        connection->disconnectFromServer();
        connection->deleteLater();
    }
}
