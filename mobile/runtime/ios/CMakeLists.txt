cmake_minimum_required(VERSION 3.22)
project(fluxrec_mobile_ios LANGUAGES C CXX)

if(NOT APPLE)
  message(FATAL_ERROR "The iOS runtime must be built with AppleClang on macOS")
endif()

# CMake's iOS toolchain may leave this empty even when
# CMAKE_OSX_ARCHITECTURES is set. Dobby uses it to select its ARM64 backend.
if(NOT CMAKE_SYSTEM_PROCESSOR)
  set(CMAKE_SYSTEM_PROCESSOR "arm64" CACHE STRING "iOS target processor" FORCE)
endif()

include(FetchContent)
set(DOBBY_DEBUG OFF CACHE BOOL "" FORCE)
FetchContent_Declare(
  dobby
  GIT_REPOSITORY https://github.com/jmpews/Dobby.git
  GIT_TAG 5dfc8546954ce3b3198132ab13fddb89ee92cdd7
)
FetchContent_MakeAvailable(dobby)

add_library(fluxrec_mobile SHARED ../common/fluxrec_runtime.cpp)
target_compile_features(fluxrec_mobile PRIVATE cxx_std_17)
target_compile_options(fluxrec_mobile PRIVATE -O2 -fvisibility=hidden -ffunction-sections -fdata-sections)
target_link_libraries(fluxrec_mobile PRIVATE dobby_static dl)
set_target_properties(fluxrec_mobile PROPERTIES
  OUTPUT_NAME fluxrec_mobile
  FRAMEWORK FALSE
  XCODE_ATTRIBUTE_CODE_SIGNING_ALLOWED NO
  XCODE_ATTRIBUTE_IPHONEOS_DEPLOYMENT_TARGET 16.0
)

set(FLUXREC_SERVER_HOST "ns.ripo-ripoteam.workers.dev" CACHE STRING "Flux Rec name-server host")
set(FLUXREC_PHOTON_REALTIME "ec2eaafc-0c8d-4e68-8f5f-fe4b1d3fb02f" CACHE STRING "Photon Realtime app id")
set(FLUXREC_PHOTON_VOICE "cfd2741f-4fab-46b3-a925-54d8bf922f0d" CACHE STRING "Photon Voice app id")
set(FLUXREC_PHOTON_CHAT "" CACHE STRING "Photon Chat app id")
set(FLUXREC_PHOTON_REGION "us" CACHE STRING "Photon fixed region")

target_compile_definitions(fluxrec_mobile PRIVATE
  FLUXREC_SERVER_HOST="${FLUXREC_SERVER_HOST}"
  FLUXREC_PHOTON_REALTIME="${FLUXREC_PHOTON_REALTIME}"
  FLUXREC_PHOTON_VOICE="${FLUXREC_PHOTON_VOICE}"
  FLUXREC_PHOTON_CHAT="${FLUXREC_PHOTON_CHAT}"
  FLUXREC_PHOTON_REGION="${FLUXREC_PHOTON_REGION}"
)
