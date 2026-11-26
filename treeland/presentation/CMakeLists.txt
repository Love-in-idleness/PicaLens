# SPDX-License-Identifier: Apache-2.0

if(NOT IS_ABSOLUTE "${PICALENS_SHADER_DIR}")
    message(FATAL_ERROR "Set PICALENS_SHADER_DIR to the absolute path to PicaLens/src/shaders")
endif()

target_sources(libtreeland PRIVATE
    "${CMAKE_CURRENT_SOURCE_DIR}/picalenspresentation.cpp"
    "${CMAKE_CURRENT_SOURCE_DIR}/picalenspresentation.h"
)

function(picalens_shader name algorithm feature)
    set(algorithm_file "${PICALENS_SHADER_DIR}/${algorithm}.glsl")
    set(pass_file "${PICALENS_SHADER_DIR}/${name}-pass.glsl")
    set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS "${algorithm_file}" "${pass_file}")
    file(READ "${algorithm_file}" shader_algorithm)
    file(READ "${pass_file}" shader_pass)
    # Qt Quick supplies texture coordinates in the scene graph's texture orientation.
    string(REPLACE "gl_FragCoord.xy" "(qt_TexCoord0 * outputSize)" shader_pass "${shader_pass}")
    set(shader_define "")
    if(feature)
        set(shader_define "#define ${feature}")
    endif()
    configure_file(Pass.frag.in "${CMAKE_CURRENT_BINARY_DIR}/${name}.frag" @ONLY)
endfunction()

picalens_shader(easu fsr1 FSR_EASU_F)
picalens_shader(rcas fsr1 FSR_RCAS_F)
picalens_shader(sgsr sgsr1 "")

qt_add_resources(libtreeland "picalens_presentation"
    PREFIX "/picalens"
    BASE "${CMAKE_CURRENT_SOURCE_DIR}"
    FILES "${CMAKE_CURRENT_SOURCE_DIR}/Presentation.qml"
)
qt_add_shaders(libtreeland "picalens_upscalers"
    PREFIX "/picalens"
    GLSL "310 es,430"
    NOHLSL
    NOMSL
    FILES
        "${CMAKE_CURRENT_SOURCE_DIR}/Presentation.vert"
        "${CMAKE_CURRENT_BINARY_DIR}/easu.frag"
        "${CMAKE_CURRENT_BINARY_DIR}/rcas.frag"
        "${CMAKE_CURRENT_BINARY_DIR}/sgsr.frag"
    OUTPUTS
        Presentation.vert.qsb
        easu.frag.qsb
        rcas.frag.qsb
        sgsr.frag.qsb
)
