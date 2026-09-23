/*
 * steam_api64_stub.c — minimal Steamworks API stub for Flux Rec.
 *
 * Last-resort fallback when the Goldberg emulator cannot be installed.
 * Exports the small flat-C SteamAPI_* surface the Rec Room client resolves
 * at startup. SteamAPI_Init reports success so the client boots past
 * "Failed to initialize Steam Platform" without any Steam client installed.
 *
 * Build (no Visual C++ runtime dependency — plain C, MinGW only):
 *   x86_64-w64-mingw32-gcc -shared -O2 -o steam_api64_stub.dll steam_api64_stub.c \
 *       -static-libgcc -Wl,--kill-at
 *
 * The resulting DLL links only against kernel32/msvcrt (always present on
 * Windows) — never vcruntime140.dll / the VC++ redist.
 */
#include <stdbool.h>
#include <stdint.h>

#if defined(_WIN32) || defined(__CYGWIN__)
#define STUB_EXPORT __declspec(dllexport)
#else
#define STUB_EXPORT
#endif

typedef uint32_t HSteamPipe;
typedef uint32_t HSteamUser;

/* The one the client actually gates on. */
STUB_EXPORT bool SteamAPI_Init(void) { return true; }

STUB_EXPORT void SteamAPI_Shutdown(void) {}

/* Returning false = "no restart needed", the game keeps booting. */
STUB_EXPORT bool SteamAPI_RestartAppIfNecessary(uint32_t unOwnAppID) {
    (void)unOwnAppID;
    return false;
}

STUB_EXPORT bool SteamAPI_IsSteamRunning(void) { return true; }

/* Plausible non-zero handles; the client only null-checks them. */
STUB_EXPORT HSteamUser SteamAPI_GetHSteamUser(void) { return 1; }
STUB_EXPORT HSteamPipe SteamAPI_GetHSteamPipe(void) { return 1; }
