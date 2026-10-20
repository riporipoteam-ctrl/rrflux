cmake_minimum_required(VERSION 3.22)
project(fluxrec_mobile LANGUAGES C CXX)

include(FetchContent)
FetchContent_Declare(
  and64inlinehook
  GIT_REPOSITORY https://github.com/Rprop/And64InlineHook.git
  GIT_TAG 2787585191552b291c508e7ae639557164441a13
)
FetchContent_MakeAvailable(and64inlinehook)

add_library(fluxrec_mobile SHARED
  ../common/fluxrec_runtime.cpp
  ${and64inlinehook_SOURCE_DIR}/And64InlineHook.cpp
)
target_include_directories(fluxrec_mobile PRIVATE ${and64inlinehook_SOURCE_DIR})
target_compile_features(fluxrec_mobile PRIVATE cxx_std_17)
target_compile_options(fluxrec_mobile PRIVATE -O2 -fvisibility=hidden -ffunction-sections -fdata-sections)
target_link_options(fluxrec_mobile PRIVATE -Wl,--gc-sections)
target_link_libraries(fluxrec_mobile PRIVATE log dl)

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
