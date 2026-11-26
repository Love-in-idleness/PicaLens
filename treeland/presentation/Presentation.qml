// SPDX-License-Identifier: Apache-2.0
import QtQuick

Item {
    id: presentation
    required property Item sourceWindow
    required property rect sourceRect
    required property string filter
    required property size sourcePixels
    required property size filterPixels
    required property rect picture
    property bool captureHidden: false
    property bool ready: false
    readonly property bool failed: filter !== "linear" && (outputShader.status === ShaderEffect.Error
                                    || (filter === "fsr" && easu.status === ShaderEffect.Error))
    readonly property string vertexShader: "qrc:/picalens/Presentation.vert.qsb"

    // Wait for the output texture even when Qt reuses a cached shader.
    Component.onCompleted: {
        if (filter === "linear")
            original.scheduleUpdate()
        else
            filtered.scheduleUpdate()
    }

    Rectangle {
        anchors.fill: parent
        color: "black"
    }

    ShaderEffectSource {
        id: original
        sourceItem: presentation.sourceWindow
        sourceRect: presentation.sourceRect
        textureSize: presentation.sourcePixels
        hideSource: presentation.captureHidden
        live: true
        visible: presentation.filter === "linear"
        x: presentation.picture.x
        y: presentation.picture.y
        width: presentation.picture.width
        height: presentation.picture.height
        onScheduledUpdateCompleted: {
            if (presentation.filter === "linear")
                presentation.ready = true
        }
    }

    ShaderEffect {
        id: easu
        visible: presentation.filter === "fsr"
        width: presentation.filterPixels.width
        height: presentation.filterPixels.height
        property var inputImage: original
        property size outputSize: presentation.filterPixels
        vertexShader: presentation.vertexShader
        fragmentShader: presentation.filter === "fsr" ? "qrc:/picalens/easu.frag.qsb" : ""
    }

    ShaderEffectSource {
        id: firstPass
        sourceItem: presentation.filter === "fsr" ? easu : null
        hideSource: true
        textureSize: presentation.filterPixels
        live: true
        visible: false
    }

    ShaderEffect {
        id: outputShader
        visible: presentation.filter !== "linear"
        width: presentation.filterPixels.width
        height: presentation.filterPixels.height
        property var inputImage: presentation.filter === "fsr" ? firstPass : original
        property size outputSize: presentation.filterPixels
        vertexShader: presentation.vertexShader
        fragmentShader: presentation.filter === "fsr" ? "qrc:/picalens/rcas.frag.qsb"
                        : presentation.filter === "sgsr" ? "qrc:/picalens/sgsr.frag.qsb" : ""
    }

    ShaderEffectSource {
        id: filtered
        visible: presentation.filter !== "linear"
        sourceItem: visible ? outputShader : null
        hideSource: true
        textureSize: presentation.filterPixels
        smooth: true
        x: presentation.picture.x
        y: presentation.picture.y
        width: presentation.picture.width
        height: presentation.picture.height
        onScheduledUpdateCompleted: presentation.ready = true
    }
}
