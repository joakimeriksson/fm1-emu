# The FM-1 plugin's AU (macOS) and VST3 (macOS, Windows), by clap-wrapper (MIT) around the CLAP
# that cargo builds. clap-wrapper downloads the SDKs it needs: the CLAP SDK (MIT), Apple's
# AudioUnitSDK (Apache-2.0) and, for VST3, Steinberg's VST3 SDK (MIT since 3.8).
# tools/build_plugin.sh (macOS) and tools/build_plugin.ps1 (Windows) drive it:
#   cmake -B build -DFM1_CLAP=/path/to/FM-1.clap && cmake --build build
# On macOS each bundle carries the CLAP. On Windows the VST3 loads FM-1.clap at run time from the
# CLAP folders (Common Files\CLAP, %LOCALAPPDATA%\Programs\Common\CLAP or CLAP_PATH).
cmake_minimum_required(VERSION 3.21)
if (APPLE)
    set(CMAKE_OSX_DEPLOYMENT_TARGET 11.0 CACHE STRING "")
    set(CMAKE_OSX_ARCHITECTURES arm64 CACHE STRING "")
endif ()
project(fm1-plugin-wrap VERSION 0.1.0 LANGUAGES C CXX)
if (APPLE)
    enable_language(OBJC OBJCXX)
endif ()
set(CMAKE_CXX_STANDARD 17)
# clap-wrapper links the MSVC runtime statically into its libraries; the wrapper targets here
# must match, or the link mixes /MT and /MD objects (LNK2038).
if (MSVC)
    set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")
endif ()

set(FM1_CLAP "" CACHE PATH "The FM-1.clap the wrappers carry (a bundle on macOS, the DLL on Windows)")
if (NOT EXISTS "${FM1_CLAP}")
    message(FATAL_ERROR "FM1_CLAP (${FM1_CLAP}) is not a .clap: run tools/build_plugin.sh or tools/build_plugin.ps1")
endif ()

set(CLAP_WRAPPER_DOWNLOAD_DEPENDENCIES TRUE CACHE BOOL "" FORCE)
include(FetchContent)
FetchContent_Declare(clap-wrapper
        GIT_REPOSITORY https://github.com/free-audio/clap-wrapper.git
        GIT_TAG v0.16.0)
FetchContent_MakeAvailable(clap-wrapper)

# An instrument ('aumu'); the codes name it to AU hosts: manufacturer Fm1e, subtype Fm1s.
if (APPLE)
    add_library(fm1_auv2 MODULE)
    target_add_auv2_wrapper(
            TARGET fm1_auv2
            OUTPUT_NAME "FM-1"
            BUNDLE_IDENTIFIER "io.github.joakimeriksson.fm1-emu.auv2"
            BUNDLE_VERSION "${PROJECT_VERSION}"
            MANUFACTURER_NAME "fm1-emu"
            MANUFACTURER_CODE "Fm1e"
            SUBTYPE_CODE "Fm1s"
            INSTRUMENT_TYPE "aumu"
            MACOS_EMBEDDED_CLAP_LOCATION "${FM1_CLAP}")
endif ()

# The VST3 (Steinberg's SDK, MIT since 3.8): on macOS the same CLAP inside; on Windows a bundle
# folder (FM-1.vst3/Contents/x86_64-win/FM-1.vst3) that loads FM-1.clap from the CLAP folders.
add_library(fm1_vst3 MODULE)
target_add_vst3_wrapper(
        TARGET fm1_vst3
        OUTPUT_NAME "FM-1"
        BUNDLE_IDENTIFIER "io.github.joakimeriksson.fm1-emu.vst3"
        BUNDLE_VERSION "${PROJECT_VERSION}"
        WINDOWS_FOLDER_VST3 TRUE
        MACOS_EMBEDDED_CLAP_LOCATION "${FM1_CLAP}")
