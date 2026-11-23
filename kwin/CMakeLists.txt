cmake_minimum_required(VERSION 3.24)
project(PicaLensKWin VERSION 0.0.1 LANGUAGES CXX)

find_package(ECM 6.13 REQUIRED NO_MODULE)
list(APPEND CMAKE_MODULE_PATH ${ECM_MODULE_PATH})
include(KDEInstallDirs)

find_package(Qt6 6.8 REQUIRED COMPONENTS DBus Widgets)
find_package(KWin 6.3.6 EXACT REQUIRED CONFIG)

set(CMAKE_CXX_STANDARD 23)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
set(CMAKE_AUTOMOC ON)
list(APPEND CMAKE_AUTOMOC_MACRO_NAMES KWIN_EFFECT_FACTORY_SUPPORTED_ENABLED)

add_library(picalens MODULE effect.cpp presentation.cpp inputfilter.cpp)
set_target_properties(picalens PROPERTIES PREFIX "")
target_link_libraries(picalens PRIVATE KWin::kwin Qt6::DBus Qt6::Widgets)
install(TARGETS picalens LIBRARY DESTINATION "${KDE_INSTALL_PLUGINDIR}/kwin/effects/plugins")
