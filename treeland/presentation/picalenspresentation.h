// SPDX-License-Identifier: Apache-2.0

#pragma once

#include <QObject>
#include <QPointer>
#include <QRectF>
#include <sys/types.h>

class QLocalServer;
class QLocalSocket;
class QQuickItem;
class QQuickTransform;
class QTimer;
class SurfaceWrapper;
class WaylandServer;

class PicaLensPresentation : public QObject
{
public:
    explicit PicaLensPresentation(WaylandServer *server);
    ~PicaLensPresentation() override;

protected:
    bool eventFilter(QObject *object, QEvent *event) override;

private:
    enum class Phase { Idle, Selecting, Preparing, Presenting };

    void listen();
    void accept();
    void readRequest();
    SurfaceWrapper *pick(const QPointF &position) const;
    bool start(SurfaceWrapper *source);
    void prepareInput();
    void fail();
    void stop();

    WaylandServer *m_server;
    QLocalServer *m_listener;
    QTimer *m_deadline;
    QPointer<QLocalSocket> m_client;
    QPointer<SurfaceWrapper> m_source;
    QPointer<QQuickItem> m_view;
    QPointer<QQuickTransform> m_transform;
    QList<QMetaObject::Connection> m_watchers;
    QRectF m_output;
    QRectF m_picture;
    QString m_filter;
    uid_t m_uid = uid_t(-1);
    qreal m_zoom = 1.;
    Qt::MouseButtons m_pressed = Qt::NoButton;
    Phase m_phase = Phase::Idle;
};
