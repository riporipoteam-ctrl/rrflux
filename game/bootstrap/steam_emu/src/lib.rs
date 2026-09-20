// Flux Rec — faithful steam_api64 emulator (clean-room, Goldberglike).
//
// Design:
//  - All 995 original named exports are preserved (checked against the
//    shipping steam_api64.dll export list).
//  - Interface accessors return real, non-null interface objects whose
//    vtables are filled with typed, signature-safe flat wrappers.
//  - Every flat wrapper is an `extern "system"` fn taking
//    (this, u64, u64, u64, u64, u64) -> u64, so any vtable slot call is
//    ABI-safe no matter the real signature (all args/return fit in regs).
//  - High-value methods (init, identity, tickets, callbacks, utils,
//    apps, friends, matchmaking, UGC, remote storage, HTTP, input, ...)
//    return plausible non-null / success values.
//  - Callback + call-result registration is honored: auth-ticket
//    responses and common async results are dispatched through
//    SteamAPI_RunCallbacks.
//  - Never touches real Steam. RestartAppIfNecessary always false.
//
// Build: cargo build --release --target x86_64-pc-windows-gnu
#![allow(non_snake_case, non_upper_case_globals, dead_code)]

use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

// ---------------------------------------------------------------- helpers

type SlotU64 = unsafe extern "system" fn(*mut c_void, u64, u64, u64, u64, u64) -> u64;

pub unsafe extern "system" fn __pad_slot(
    _s: *mut c_void, _a: u64, _b: u64, _c: u64, _d: u64, _e: u64,
) -> u64 {
    0
}

#[repr(C)]
struct IfaceObj {
    vtable: *const c_void,
}
unsafe impl Sync for IfaceObj {}
unsafe impl Send for IfaceObj {}

// Deterministic fake identity, stable across process runs.
const FAKE_STEAM_ID: u64 = 76561197960265728 + 0x465558; // 7656119 xxxx (universe 1, account type individual)
const FAKE_APP_ID: u32 = 223750; // Rec Room's real app id
const BUILD_ID: u32 = 8475386;   // Nov 2022 era build id marker

static CALL_ID: AtomicU64 = AtomicU64::new(0x46554C58_0000_0001);
static TICKET_COUNTER: AtomicU32 = AtomicU32::new(0x1000);

fn next_call() -> u64 {
    CALL_ID.fetch_add(1, Ordering::Relaxed)
}

// Read a C string into a Rust String (lossy). Null-safe.
unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    CStr::from_ptr(p).to_string_lossy().into_owned()
}

// ---------------------------------------------------------------- callbacks

// Steam callback message ids we dispatch (real values):
//  101 = SteamServersConnected_t, 113 = ValidateAuthTicketResponse_t,
//  163 = GetAuthSessionTicketResponse_t, 703 = PersonaStateChange_t
const CB_STEAM_SERVERS_CONNECTED: i32 = 101;
const CB_VALIDATE_AUTH_TICKET: i32 = 113;
const CB_AUTH_SESSION_TICKET: i32 = 163;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CallbackMsg {
    m_hSteamUser: i32,
    m_iCallback: i32,
    m_pubParam: *mut u8,
    m_cubParam: i32,
}
unsafe impl Send for CallbackMsg {}
unsafe impl Sync for CallbackMsg {}

struct CbReg {
    user: i32,
    callback: i32,
    func: *mut c_void,
    call: u64, // 0 = persistent callback, else call-result handle
}
unsafe impl Send for CbReg {}
unsafe impl Sync for CbReg {}

static REG_LOCK: std::sync::Mutex<Vec<CbReg>> = std::sync::Mutex::new(Vec::new());

struct Queued {
    user: i32,
    cb: i32,
    call: u64, // SteamAPICall this payload belongs to (0 if none)
    ptr: *mut u8,
    len: usize,
}
unsafe impl Send for Queued {}
unsafe impl Sync for Queued {}

static QUEUE_LOCK: std::sync::Mutex<Vec<Queued>> = std::sync::Mutex::new(Vec::new());

fn queue_callback(user: i32, cb: i32, call: u64, payload: &[u8]) {
    let boxed = payload.to_vec().into_boxed_slice();
    let ptr = boxed.as_ptr() as *mut u8;
    let len = boxed.len();
    std::mem::forget(boxed);
    let mut q = QUEUE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    q.push(Queued { user, cb, call, ptr, len });
}

// Returns a pointer to static NUL-terminated bytes (for const char* returns).
fn static_name(s: &'static [u8]) -> u64 {
    s.as_ptr() as u64
}

// Dispatch every queued callback to matching registered callbacks and
// call-results, then free the payloads.
fn dispatch_queued() {
    let items: Vec<Queued> = {
        let mut q = QUEUE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *q)
    };
    for item in items {
        let regs = REG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for r in regs.iter() {
            let matches = (r.call == 0 && r.callback == item.cb)
                || (r.call != 0 && r.call == item.call && item.call != 0);
            if matches && !r.func.is_null() {
                unsafe {
                    let vtbl = *(r.func as *const *const c_void);
                    if !vtbl.is_null() {
                        let run_raw = *(vtbl as *const *const c_void);
                        let run: unsafe extern "system" fn(*mut c_void, *mut c_void) =
                            std::mem::transmute(run_raw);
                        run(r.func, item.ptr as *mut c_void);
                    }
                }
            }
        }
        unsafe {
            drop(Box::from_raw(std::slice::from_raw_parts_mut(item.ptr, item.len)));
        }
    }
}

// ---------------------------------------------------------------- interface macro
//
// def_iface! { VTable, VTABLE_STATIC, OBJ_STATIC, ps, pa, pb, pc, pd, pe, IFACE_FLAT_NAME,
//   Method = FlatExportName { body } ... }
//
// Generates: typed #[no_mangle] flat exports, a #[repr(C)] vtable struct,
// a static vtable (slots in declaration order + 128 pad slots), and a
// static interface object pointing at the vtable.

macro_rules! def_iface {
    ($vt:ident, $vs:ident, $obj:ident, $ps:ident, $pa:ident, $pb:ident, $pc:ident, $pd:ident, $pe:ident,
     $( $method:ident = $flat:ident $body:block ),* $(,)?) => {
        $(
            #[no_mangle]
            #[allow(unused_variables)]
            pub unsafe extern "system" fn $flat(
                $ps: *mut c_void, $pa: u64, $pb: u64, $pc: u64, $pd: u64, $pe: u64,
            ) -> u64 $body
        )*
        #[repr(C)]
        pub struct $vt {
            $( pub $method: SlotU64, )*
            pub __pad: [SlotU64; 128],
        }
        static $vs: $vt = $vt {
            $( $method: $flat, )*
            __pad: [__pad_slot; 128],
        };
        static $obj: IfaceObj = IfaceObj {
            vtable: &$vs as *const $vt as *const c_void,
        };
    }
}

// ---------------------------------------------------------------- ISteamClient

def_iface! { VClient, VCLIENT, CLIENT_OBJ, ps, pa, pb, pc, pd, pe,
    CreateSteamPipe = SteamAPI_ISteamClient_CreateSteamPipe { { 1 } },
    BReleaseSteamPipe = SteamAPI_ISteamClient_BReleaseSteamPipe { { 1 } },
    ConnectToGlobalUser = SteamAPI_ISteamClient_ConnectToGlobalUser { { 1 } },
    CreateLocalUser = SteamAPI_ISteamClient_CreateLocalUser { { 1 } },
    ReleaseUser = SteamAPI_ISteamClient_ReleaseUser { { 0 } },
    GetISteamUser = SteamAPI_ISteamClient_GetISteamUser { { &USER_OBJ as *const _ as u64 } },
    GetISteamGameServer = SteamAPI_ISteamClient_GetISteamGameServer { { &GAMESERVER_OBJ as *const _ as u64 } },
    SetLocalIPBinding = SteamAPI_ISteamClient_SetLocalIPBinding { { 0 } },
    GetISteamFriends = SteamAPI_ISteamClient_GetISteamFriends { { &FRIENDS_OBJ as *const _ as u64 } },
    GetISteamUtils = SteamAPI_ISteamClient_GetISteamUtils { { &UTILS_OBJ as *const _ as u64 } },
    GetISteamMatchmaking = SteamAPI_ISteamClient_GetISteamMatchmaking { { &MATCHMAKING_OBJ as *const _ as u64 } },
    GetISteamMatchmakingServers = SteamAPI_ISteamClient_GetISteamMatchmakingServers { { &MMSERVERS_OBJ as *const _ as u64 } },
    GetISteamGameSearch = SteamAPI_ISteamClient_GetISteamGameSearch { { &GAMESEARCH_OBJ as *const _ as u64 } },
    GetISteamUserStats = SteamAPI_ISteamClient_GetISteamUserStats { { &USERSTATS_OBJ as *const _ as u64 } },
    GetISteamGameServerStats = SteamAPI_ISteamClient_GetISteamGameServerStats { { &GAMESERVERSTATS_OBJ as *const _ as u64 } },
    GetISteamApps = SteamAPI_ISteamClient_GetISteamApps { { &APPS_OBJ as *const _ as u64 } },
    GetISteamNetworking = SteamAPI_ISteamClient_GetISteamNetworking { { &NETWORKING_OBJ as *const _ as u64 } },
    GetISteamRemoteStorage = SteamAPI_ISteamClient_GetISteamRemoteStorage { { &REMOTESTORAGE_OBJ as *const _ as u64 } },
    GetISteamScreenshots = SteamAPI_ISteamClient_GetISteamScreenshots { { &SCREENSHOTS_OBJ as *const _ as u64 } },
    GetISteamHTTP = SteamAPI_ISteamClient_GetISteamHTTP { { &HTTP_OBJ as *const _ as u64 } },
    GetISteamController = SteamAPI_ISteamClient_GetISteamController { { &CONTROLLER_OBJ as *const _ as u64 } },
    GetISteamUGC = SteamAPI_ISteamClient_GetISteamUGC { { &UGC_OBJ as *const _ as u64 } },
    GetISteamAppList = SteamAPI_ISteamClient_GetISteamAppList { { &APPLIST_OBJ as *const _ as u64 } },
    GetISteamMusic = SteamAPI_ISteamClient_GetISteamMusic { { &MUSIC_OBJ as *const _ as u64 } },
    GetISteamMusicRemote = SteamAPI_ISteamClient_GetISteamMusicRemote { { &MUSICREMOTE_OBJ as *const _ as u64 } },
    GetISteamHTMLSurface = SteamAPI_ISteamClient_GetISteamHTMLSurface { { &HTMLSURFACE_OBJ as *const _ as u64 } },
    GetISteamInventory = SteamAPI_ISteamClient_GetISteamInventory { { &INVENTORY_OBJ as *const _ as u64 } },
    GetISteamVideo = SteamAPI_ISteamClient_GetISteamVideo { { &VIDEO_OBJ as *const _ as u64 } },
    GetISteamParentalSettings = SteamAPI_ISteamClient_GetISteamParentalSettings { { &PARENTAL_OBJ as *const _ as u64 } },
    GetISteamInput = SteamAPI_ISteamClient_GetISteamInput { { &INPUT_OBJ as *const _ as u64 } },
    GetISteamParties = SteamAPI_ISteamClient_GetISteamParties { { &PARTIES_OBJ as *const _ as u64 } },
    GetISteamRemotePlay = SteamAPI_ISteamClient_GetISteamRemotePlay { { &REMOTEPLAY_OBJ as *const _ as u64 } },
    SetWarningMessageHook = SteamAPI_ISteamClient_SetWarningMessageHook { { 0 } },
    BShutdownIfAllPipesClosed = SteamAPI_ISteamClient_BShutdownIfAllPipesClosed { { 1 } },
    GetIPCCallCount = SteamAPI_ISteamClient_GetIPCCallCount { { 0 } },
    GetISteamGenericInterface = SteamAPI_ISteamClient_GetISteamGenericInterface {
        {
            // a = HSteamUser, b = HSteamPipe, c = version string
            let ver = cstr(pc as *const c_char);
            find_interface(&ver) as u64
        }
    },
}

// ---------------------------------------------------------------- ISteamUser
def_iface! { VUser, VUSER, USER_OBJ, ps, pa, pb, pc, pd, pe,
    GetSteamID = SteamAPI_ISteamUser_GetSteamID { { FAKE_STEAM_ID } },
    BLoggedOn = SteamAPI_ISteamUser_BLoggedOn { { 1 } },
    GetHSteamUser = SteamAPI_ISteamUser_GetHSteamUser { { 1 } },
    InitiateGameConnection = SteamAPI_ISteamUser_InitiateGameConnection {
        {
            // a = blob ptr (out), b = max blob, c = steamIDGameServer, d/e = ip/port, secure
            // Fill a plausible auth blob so the server path can proceed.
            let n = TICKET_COUNTER.fetch_add(1, Ordering::Relaxed);
            if pa != 0 && pb >= 16 {
                let p = pa as *mut u8;
                std::ptr::write_bytes(p, 0, pb as usize);
                std::ptr::copy_nonoverlapping(b"FLUXREC-AUTH-BLOB-".as_ptr(), p, 18.min(pb as usize));
                std::ptr::write(p.add(20), (n & 0xFF) as u8);
                std::ptr::write(p.add(21), ((n >> 8) & 0xFF) as u8);
                std::ptr::write(p.add(22), ((n >> 16) & 0xFF) as u8);
            }
            32 // blob length
        }
    },
    TerminateGameConnection = SteamAPI_ISteamUser_TerminateGameConnection { { 0 } },
    TrackAppUsageEvent = SteamAPI_ISteamUser_TrackAppUsageEvent { { 0 } },
    GetUserDataFolder = SteamAPI_ISteamUser_GetUserDataFolder {
        {
            // a = out buffer, b = buffer size. Return buffer on success.
            if pa != 0 && pb > 16 {
                let s = b"FluxRec\0";
                let n = s.len().min(pb as usize);
                std::ptr::copy_nonoverlapping(s.as_ptr(), pa as *mut u8, n);
                return pa;
            }
            0
        }
    },
    StartVoiceRecording = SteamAPI_ISteamUser_StartVoiceRecording { { 0 } },
    StopVoiceRecording = SteamAPI_ISteamUser_StopVoiceRecording { { 0 } },
    GetAvailableVoice = SteamAPI_ISteamUser_GetAvailableVoice {
        {
            // a..d = out ptrs for compressed/uncompressed bytes + sample rate
            if pc != 0 { *(pc as *mut u32) = 0; }
            if pe != 0 { *(pe as *mut u32) = 0; }
            0 // k_EVoiceResultNotRecording
        }
    },
    GetVoice = SteamAPI_ISteamUser_GetVoice {
        {
            if pc != 0 { *(pc as *mut u32) = 0; }
            if pe != 0 { *(pe as *mut u32) = 0; }
            0
        }
    },
    DecompressVoice = SteamAPI_ISteamUser_DecompressVoice {
        {
            if pd != 0 { *(pd as *mut u32) = 0; }
            0
        }
    },
    GetVoiceOptimalSampleRate = SteamAPI_ISteamUser_GetVoiceOptimalSampleRate { { 48000 } },
    GetAuthSessionTicket = SteamAPI_ISteamUser_GetAuthSessionTicket {
        {
            // a = ticket buffer (out), b = buffer size, c = out ticket-size ptr,
            // d = identity ptr (optional). Real sig:
            // HAuthTicket GetAuthSessionTicket(void*, int, uint32*, const SteamNetworkingIdentity*)
            let n = TICKET_COUNTER.fetch_add(1, Ordering::Relaxed);
            let size: u32 = 100;
            if pa != 0 && (pb as u32) >= size {
                let p = pa as *mut u8;
                std::ptr::write_bytes(p, 0, size as usize);
                std::ptr::copy_nonoverlapping(b"FLUXREC-TICKET-v1".as_ptr(), p, 17);
                std::ptr::write(p.add(20), (n & 0xFF) as u8);
                std::ptr::write(p.add(21), ((n >> 8) & 0xFF) as u8);
                std::ptr::write(p.add(22), ((n >> 16) & 0xFF) as u8);
                std::ptr::write(p.add(23), ((n >> 24) & 0xFF) as u8);
            }
            if pc != 0 {
                *(pc as *mut u32) = size;
            }
            // Dispatch the ticket-response callback like real Steam does.
            let ticket_id = 0x46554C58_0000_0000u64 | (n as u64);
            #[repr(C, packed)]
            struct TicketResp { m_hAuthTicket: u32, m_eResult: i32 }
            let resp = TicketResp { m_hAuthTicket: ticket_id as u32, m_eResult: 1 };
            let raw = unsafe {
                std::slice::from_raw_parts(&resp as *const _ as *const u8,
                    std::mem::size_of::<TicketResp>())
            };
            queue_callback(1, CB_AUTH_SESSION_TICKET, next_call(), raw);
            ticket_id
        }
    },
    BeginAuthSession = SteamAPI_ISteamUser_BeginAuthSession {
        {
            // a = ticket ptr, b = ticket size, c = steamID -> k_EBeginAuthSessionResultOK
            let user = (pc & 0xFFFFFFFF) as i32;
            // Real Steam validates asynchronously; emulate the callback.
            #[repr(C, packed)]
            struct ValidateResp { m_SteamID: u64, m_eAuthSessionResponse: i32, m_OwnerSteamID: u64 }
            let resp = ValidateResp {
                m_SteamID: pc, m_eAuthSessionResponse: 0, m_OwnerSteamID: FAKE_STEAM_ID,
            };
            let raw = unsafe {
                std::slice::from_raw_parts(&resp as *const _ as *const u8,
                    std::mem::size_of::<ValidateResp>())
            };
            queue_callback(user, CB_VALIDATE_AUTH_TICKET, next_call(), raw);
            0
        }
    },
    EndAuthSession = SteamAPI_ISteamUser_EndAuthSession { { 0 } },
    CancelAuthTicket = SteamAPI_ISteamUser_CancelAuthTicket { { 0 } },
    UserHasLicenseForApp = SteamAPI_ISteamUser_UserHasLicenseForApp { { 1 } },
    BIsBehindNAT = SteamAPI_ISteamUser_BIsBehindNAT { { 0 } },
    AdvertiseGame = SteamAPI_ISteamUser_AdvertiseGame { { 0 } },
    RequestEncryptedAppTicket = SteamAPI_ISteamUser_RequestEncryptedAppTicket {
        {
            // a = data ptr, b = data size -> async SteamAPICall; queue completion
            let call = next_call();
            let id = next_call();
            #[repr(C, packed)]
            struct EncResp { m_eResult: i32 }
            let resp = EncResp { m_eResult: 1 };
            let raw = unsafe {
                std::slice::from_raw_parts(&resp as *const _ as *const u8,
                    std::mem::size_of::<EncResp>())
            };
            queue_callback(1, 155 /* EncryptedAppTicketResponse_t */, call, raw);
            let _ = id;
            call
        }
    },
    GetEncryptedAppTicket = SteamAPI_ISteamUser_GetEncryptedAppTicket {
        {
            // a = buffer, b = size, c = out size -> return true if buffer given
            if pa != 0 && pb >= 32 {
                let p = pa as *mut u8;
                std::ptr::write_bytes(p, 0, 32);
                std::ptr::copy_nonoverlapping(b"FLUXREC-ENCRYPTED-APP-TICKET".as_ptr(), p, 28);
                if pc != 0 { *(pc as *mut u32) = 32; }
                return 1;
            }
            if pc != 0 { *(pc as *mut u32) = 32; }
            0
        }
    },
    GetGameBadgeLevel = SteamAPI_ISteamUser_GetGameBadgeLevel { { 0 } },
    GetPlayerSteamLevel = SteamAPI_ISteamUser_GetPlayerSteamLevel { { 10 } },
    RequestStoreAuthURL = SteamAPI_ISteamUser_RequestStoreAuthURL { { next_call() } },
    BIsPhoneVerified = SteamAPI_ISteamUser_BIsPhoneVerified { { 1 } },
    BIsTwoFactorEnabled = SteamAPI_ISteamUser_BIsTwoFactorEnabled { { 0 } },
    BIsPhoneIdentifying = SteamAPI_ISteamUser_BIsPhoneIdentifying { { 0 } },
    BIsPhoneRequiringVerification = SteamAPI_ISteamUser_BIsPhoneRequiringVerification { { 0 } },
    GetMarketEligibility = SteamAPI_ISteamUser_GetMarketEligibility { { next_call() } },
    GetDurationControl = SteamAPI_ISteamUser_GetDurationControl { { next_call() } },
}

// ---------------------------------------------------------------- ISteamFriends

def_iface! { VFriends, VFRIENDS, FRIENDS_OBJ, ps, pa, pb, pc, pd, pe,
    GetPersonaName = SteamAPI_ISteamFriends_GetPersonaName { { static_name(b"FluxRec Player\0") } },
    SetPersonaName = SteamAPI_ISteamFriends_SetPersonaName { { next_call() } },
    GetPersonaState = SteamAPI_ISteamFriends_GetPersonaState { { 1 } },
    GetFriendCount = SteamAPI_ISteamFriends_GetFriendCount { { 0 } },
    GetFriendByIndex = SteamAPI_ISteamFriends_GetFriendByIndex { { 0 } },
    GetFriendRelationship = SteamAPI_ISteamFriends_GetFriendRelationship { { 0 } },
    GetFriendPersonaState = SteamAPI_ISteamFriends_GetFriendPersonaState { { 0 } },
    GetFriendPersonaName = SteamAPI_ISteamFriends_GetFriendPersonaName { { static_name(b"\0") } },
    GetFriendGamePlayed = SteamAPI_ISteamFriends_GetFriendGamePlayed { { 0 } },
    GetFriendPersonaNameHistory = SteamAPI_ISteamFriends_GetFriendPersonaNameHistory { { static_name(b"\0") } },
    GetFriendSteamLevel = SteamAPI_ISteamFriends_GetFriendSteamLevel { { 0 } },
    GetPlayerNickname = SteamAPI_ISteamFriends_GetPlayerNickname { { static_name(b"\0") } },
    GetFriendsGroupCount = SteamAPI_ISteamFriends_GetFriendsGroupCount { { 0 } },
    GetFriendsGroupIDByIndex = SteamAPI_ISteamFriends_GetFriendsGroupIDByIndex { { 0 } },
    GetFriendsGroupName = SteamAPI_ISteamFriends_GetFriendsGroupName { { static_name(b"\0") } },
    GetFriendsGroupMembersCount = SteamAPI_ISteamFriends_GetFriendsGroupMembersCount { { 0 } },
    GetFriendsGroupMembersList = SteamAPI_ISteamFriends_GetFriendsGroupMembersList { { 0 } },
    HasFriend = SteamAPI_ISteamFriends_HasFriend { { 0 } },
    GetFriendCountFromSource = SteamAPI_ISteamFriends_GetFriendCountFromSource { { 0 } },
    GetFriendFromSourceByIndex = SteamAPI_ISteamFriends_GetFriendFromSourceByIndex { { 0 } },
    IsUserInSource = SteamAPI_ISteamFriends_IsUserInSource { { 0 } },
    SetInGameVoiceSpeaking = SteamAPI_ISteamFriends_SetInGameVoiceSpeaking { { 0 } },
    ClearRichPresence = SteamAPI_ISteamFriends_ClearRichPresence { { 0 } },
    SetRichPresence = SteamAPI_ISteamFriends_SetRichPresence { { 0 } },
    RequestFriendRichPresence = SteamAPI_ISteamFriends_RequestFriendRichPresence { { 0 } },
    GetFriendRichPresence = SteamAPI_ISteamFriends_GetFriendRichPresence { { static_name(b"\0") } },
    GetFriendRichPresenceKeyCount = SteamAPI_ISteamFriends_GetFriendRichPresenceKeyCount { { 0 } },
    GetFriendRichPresenceKeyByIndex = SteamAPI_ISteamFriends_GetFriendRichPresenceKeyByIndex { { static_name(b"\0") } },
    RequestUserInformation = SteamAPI_ISteamFriends_RequestUserInformation { { 0 } },
    InviteUserToGame = SteamAPI_ISteamFriends_InviteUserToGame { { 0 } },
    GetCoplayFriendCount = SteamAPI_ISteamFriends_GetCoplayFriendCount { { 0 } },
    GetCoplayFriend = SteamAPI_ISteamFriends_GetCoplayFriend { { 0 } },
    GetFriendCoplayTime = SteamAPI_ISteamFriends_GetFriendCoplayTime { { 0 } },
    GetFriendCoplayGame = SteamAPI_ISteamFriends_GetFriendCoplayGame { { 0 } },
    JoinClanChatRoom = SteamAPI_ISteamFriends_JoinClanChatRoom { { next_call() } },
    LeaveClanChatRoom = SteamAPI_ISteamFriends_LeaveClanChatRoom { { 0 } },
    GetClanCount = SteamAPI_ISteamFriends_GetClanCount { { 0 } },
    GetClanByIndex = SteamAPI_ISteamFriends_GetClanByIndex { { 0 } },
    GetClanName = SteamAPI_ISteamFriends_GetClanName { { static_name(b"\0") } },
    GetClanTag = SteamAPI_ISteamFriends_GetClanTag { { static_name(b"\0") } },
    GetClanChatMemberCount = SteamAPI_ISteamFriends_GetClanChatMemberCount { { 0 } },
    GetChatMemberByIndex = SteamAPI_ISteamFriends_GetChatMemberByIndex { { 0 } },
    SendClanChatMessage = SteamAPI_ISteamFriends_SendClanChatMessage { { 0 } },
    GetClanChatMessage = SteamAPI_ISteamFriends_GetClanChatMessage { { 0 } },
    IsClanChatAdmin = SteamAPI_ISteamFriends_IsClanChatAdmin { { 0 } },
    IsClanChatWindowOpenInSteam = SteamAPI_ISteamFriends_IsClanChatWindowOpenInSteam { { 0 } },
    OpenClanChatWindowInSteam = SteamAPI_ISteamFriends_OpenClanChatWindowInSteam { { 0 } },
    CloseClanChatWindowInSteam = SteamAPI_ISteamFriends_CloseClanChatWindowInSteam { { 0 } },
    SetListenForFriendsMessages = SteamAPI_ISteamFriends_SetListenForFriendsMessages { { 0 } },
    ReplyToFriendMessage = SteamAPI_ISteamFriends_ReplyToFriendMessage { { 0 } },
    GetFriendMessage = SteamAPI_ISteamFriends_GetFriendMessage { { 0 } },
    GetFollowerCount = SteamAPI_ISteamFriends_GetFollowerCount { { next_call() } },
    IsFollowing = SteamAPI_ISteamFriends_IsFollowing { { next_call() } },
    EnumerateFollowingList = SteamAPI_ISteamFriends_EnumerateFollowingList { { next_call() } },
    IsClanPublic = SteamAPI_ISteamFriends_IsClanPublic { { 0 } },
    IsClanOfficialGameGroup = SteamAPI_ISteamFriends_IsClanOfficialGameGroup { { 0 } },
    GetNumChatsWithUnreadPriorityMessages = SteamAPI_ISteamFriends_GetNumChatsWithUnreadPriorityMessages { { 0 } },
    ActivateGameOverlay = SteamAPI_ISteamFriends_ActivateGameOverlay { { 0 } },
    ActivateGameOverlayToUser = SteamAPI_ISteamFriends_ActivateGameOverlayToUser { { 0 } },
    ActivateGameOverlayToWebPage = SteamAPI_ISteamFriends_ActivateGameOverlayToWebPage { { 0 } },
    ActivateGameOverlayToStore = SteamAPI_ISteamFriends_ActivateGameOverlayToStore { { 0 } },
    SetPlayedWith = SteamAPI_ISteamFriends_SetPlayedWith { { 0 } },
    ActivateGameOverlayInviteDialog = SteamAPI_ISteamFriends_ActivateGameOverlayInviteDialog { { 0 } },
    GetSmallFriendAvatar = SteamAPI_ISteamFriends_GetSmallFriendAvatar { { 0 } },
    GetMediumFriendAvatar = SteamAPI_ISteamFriends_GetMediumFriendAvatar { { 0 } },
    GetLargeFriendAvatar = SteamAPI_ISteamFriends_GetLargeFriendAvatar { { 0 } },
    DownloadClanActivityCounts = SteamAPI_ISteamFriends_DownloadClanActivityCounts { { 0 } },
    GetClanActivityCounts = SteamAPI_ISteamFriends_GetClanActivityCounts { { 0 } },
    GetClanOfficerByIndex = SteamAPI_ISteamFriends_GetClanOfficerByIndex { { 0 } },
    GetClanOwner = SteamAPI_ISteamFriends_GetClanOwner { { 0 } },
    GetClanOfficerCount = SteamAPI_ISteamFriends_GetClanOfficerCount { { 0 } },
    RequestClanOfficerList = SteamAPI_ISteamFriends_RequestClanOfficerList { { next_call() } },
    GetUserRestrictions = SteamAPI_ISteamFriends_GetUserRestrictions { { 0 } },
    ActivateGameOverlayRemotePlayTogetherInviteDialog = SteamAPI_ISteamFriends_ActivateGameOverlayRemotePlayTogetherInviteDialog { { 0 } },
}

// ---------------------------------------------------------------- ISteamUtils
// ---------------------------------------------------------------- ISteamUtils

def_iface! { VUtils, VUTILS, UTILS_OBJ, ps, pa, pb, pc, pd, pe,
    GetSecondsSinceAppActive = SteamAPI_ISteamUtils_GetSecondsSinceAppActive { { 60 } },
    GetSecondsSinceComputerActive = SteamAPI_ISteamUtils_GetSecondsSinceComputerActive { { 120 } },
    GetConnectedUniverse = SteamAPI_ISteamUtils_GetConnectedUniverse { { 1 } },
    GetServerRealTime = SteamAPI_ISteamUtils_GetServerRealTime { { 1664841600 } },
    GetIPCountry = SteamAPI_ISteamUtils_GetIPCountry { { static_name(b"US\0") } },
    GetImageSize = SteamAPI_ISteamUtils_GetImageSize { { 0 } },
    GetImageRGBA = SteamAPI_ISteamUtils_GetImageRGBA { { 0 } },
    GetCSERIPPort = SteamAPI_ISteamUtils_GetCSERIPPort { { 0 } },
    GetCurrentBatteryPower = SteamAPI_ISteamUtils_GetCurrentBatteryPower { { 255 } },
    GetAppID = SteamAPI_ISteamUtils_GetAppID { { FAKE_APP_ID as u64 } },
    SetOverlayNotificationPosition = SteamAPI_ISteamUtils_SetOverlayNotificationPosition { { 0 } },
    IsAPICallCompleted = SteamAPI_ISteamUtils_IsAPICallCompleted { { 1 } },
    GetAPICallFailureReason = SteamAPI_ISteamUtils_GetAPICallFailureReason { { 0 } },
    GetAPICallResult = SteamAPI_ISteamUtils_GetAPICallResult {
        {
            // a = call handle, b = out buffer, c = buffer size,
            // d = expected callback id, e = out failed flag -> true if given
            if pe != 0 { *(pe as *mut u8) = 0; }
            1
        }
    },
    GetIPCCallCount = SteamAPI_ISteamUtils_GetIPCCallCount { { 0 } },
    SetWarningMessageHook = SteamAPI_ISteamUtils_SetWarningMessageHook { { 0 } },
    IsOverlayEnabled = SteamAPI_ISteamUtils_IsOverlayEnabled { { 0 } },
    BOverlayNeedsPresent = SteamAPI_ISteamUtils_BOverlayNeedsPresent { { 0 } },
    CheckFileSignature = SteamAPI_ISteamUtils_CheckFileSignature { { next_call() } },
    ShowGamepadTextInput = SteamAPI_ISteamUtils_ShowGamepadTextInput { { 0 } },
    GetEnteredGamepadTextLength = SteamAPI_ISteamUtils_GetEnteredGamepadTextLength { { 0 } },
    GetEnteredGamepadTextInput = SteamAPI_ISteamUtils_GetEnteredGamepadTextInput { { 0 } },
    GetSteamUILanguage = SteamAPI_ISteamUtils_GetSteamUILanguage { { static_name(b"english\0") } },
    IsSteamInBigPictureMode = SteamAPI_ISteamUtils_IsSteamInBigPictureMode { { 0 } },
    StartVRDashboard = SteamAPI_ISteamUtils_StartVRDashboard { { 0 } },
    IsVRHeadsetStreamingEnabled = SteamAPI_ISteamUtils_IsVRHeadsetStreamingEnabled { { 0 } },
    SetVRHeadsetStreamingEnabled = SteamAPI_ISteamUtils_SetVRHeadsetStreamingEnabled { { 0 } },
    IsSteamChinaLauncher = SteamAPI_ISteamUtils_IsSteamChinaLauncher { { 0 } },
    InitFilterText = SteamAPI_ISteamUtils_InitFilterText { { 1 } },
    FilterText = SteamAPI_ISteamUtils_FilterText {
        {
            // a = out buffer, b = out size, c = input, d = eFilterChar, e = out filtered count
            let src = cstr(pc as *const c_char);
            if pa != 0 && pb > 0 {
                let bytes = src.as_bytes();
                let n = bytes.len().min((pb as usize).saturating_sub(1));
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), pa as *mut u8, n);
                std::ptr::write((pa as *mut u8).add(n), 0);
            }
            if pe != 0 { *(pe as *mut u32) = 0; }
            src.len() as u64
        }
    },
    SetOverlayNotificationInset = SteamAPI_ISteamUtils_SetOverlayNotificationInset { { 0 } },
    IsSteamRunningInVR = SteamAPI_ISteamUtils_IsSteamRunningInVR { { 0 } },
    GetIPv6ConnectivityState = SteamAPI_ISteamUtils_GetIPv6ConnectivityState { { 0 } },
}

// ---------------------------------------------------------------- ISteamApps
def_iface! { VApps, VAPPS, APPS_OBJ, ps, pa, pb, pc, pd, pe,
    BIsSubscribed = SteamAPI_ISteamApps_BIsSubscribed { { 1 } },
    BIsLowViolence = SteamAPI_ISteamApps_BIsLowViolence { { 0 } },
    BIsCybercafe = SteamAPI_ISteamApps_BIsCybercafe { { 0 } },
    BIsVACBanned = SteamAPI_ISteamApps_BIsVACBanned { { 0 } },
    GetCurrentGameLanguage = SteamAPI_ISteamApps_GetCurrentGameLanguage { { static_name(b"english\0") } },
    GetAvailableGameLanguages = SteamAPI_ISteamApps_GetAvailableGameLanguages { { static_name(b"english\0") } },
    BIsSubscribedApp = SteamAPI_ISteamApps_BIsSubscribedApp { { 1 } },
    BIsDlcInstalled = SteamAPI_ISteamApps_BIsDlcInstalled { { 1 } },
    GetEarliestPurchaseUnixTime = SteamAPI_ISteamApps_GetEarliestPurchaseUnixTime { { 1664841600 } },
    BIsSubscribedFromFreeWeekend = SteamAPI_ISteamApps_BIsSubscribedFromFreeWeekend { { 0 } },
    GetDLCCount = SteamAPI_ISteamApps_GetDLCCount { { 0 } },
    GetAppBuildId = SteamAPI_ISteamApps_GetAppBuildId { { BUILD_ID as u64 } },
    GetAppInstallDir = SteamAPI_ISteamApps_GetAppInstallDir {
        {
            // a = app id, b = out buffer, c = buffer size -> install dir
            if pb != 0 && pc > 8 {
                let s = b"FluxRec\\game\0";
                let n = s.len().min(pc as usize);
                std::ptr::copy_nonoverlapping(s.as_ptr(), pb as *mut u8, n);
                return pc;
            }
            0
        }
    },
    BIsAppInstalled = SteamAPI_ISteamApps_BIsAppInstalled { { 1 } },
    GetAppOwner = SteamAPI_ISteamApps_GetAppOwner { { FAKE_STEAM_ID } },
    GetLaunchQueryParam = SteamAPI_ISteamApps_GetLaunchQueryParam { { static_name(b"\0") } },
    GetDlcDownloadProgress = SteamAPI_ISteamApps_GetDlcDownloadProgress {
        {
            if pb != 0 { *(pb as *mut u64) = 0; }
            if pc != 0 { *(pc as *mut u64) = 0; }
            1
        }
    },
    BGetDLCDataByIndex = SteamAPI_ISteamApps_BGetDLCDataByIndex { { 0 } },
    InstallDLC = SteamAPI_ISteamApps_InstallDLC { { 0 } },
    UninstallDLC = SteamAPI_ISteamApps_UninstallDLC { { 0 } },
    RequestAppProofOfPurchaseKey = SteamAPI_ISteamApps_RequestAppProofOfPurchaseKey { { 0 } },
    GetCurrentBetaName = SteamAPI_ISteamApps_GetCurrentBetaName { { 0 } },
    MarkContentCorrupt = SteamAPI_ISteamApps_MarkContentCorrupt { { 0 } },
    GetInstalledDepots = SteamAPI_ISteamApps_GetInstalledDepots {
        {
            // a = appid, b = out array, c = max -> write one depot id
            if pb != 0 && pc >= 1 {
                *(pb as *mut u32) = 223751;
                return 1;
            }
            1
        }
    },
    RequestAllProofOfPurchaseKeys = SteamAPI_ISteamApps_RequestAllProofOfPurchaseKeys { { 0 } },
    GetFileDetails = SteamAPI_ISteamApps_GetFileDetails { { next_call() } },
    GetLaunchCommandLine = SteamAPI_ISteamApps_GetLaunchCommandLine { { 0 } },
    BIsSubscribedFromFamilySharing = SteamAPI_ISteamApps_BIsSubscribedFromFamilySharing { { 0 } },
}

// ---------------------------------------------------------------- ISteamMatchmaking
def_iface! { VMatchmaking, VMATCHMAKING, MATCHMAKING_OBJ, ps, pa, pb, pc, pd, pe,
    GetFavoriteGameCount = SteamAPI_ISteamMatchmaking_GetFavoriteGameCount { { 0 } },
    GetFavoriteGame = SteamAPI_ISteamMatchmaking_GetFavoriteGame { { 0 } },
    AddFavoriteGame = SteamAPI_ISteamMatchmaking_AddFavoriteGame { { 0 } },
    RemoveFavoriteGame = SteamAPI_ISteamMatchmaking_RemoveFavoriteGame { { 0 } },
    RequestLobbyList = SteamAPI_ISteamMatchmaking_RequestLobbyList { { next_call() } },
    AddRequestLobbyListStringFilter = SteamAPI_ISteamMatchmaking_AddRequestLobbyListStringFilter { { 0 } },
    AddRequestLobbyListNumericalFilter = SteamAPI_ISteamMatchmaking_AddRequestLobbyListNumericalFilter { { 0 } },
    AddRequestLobbyListNearValueFilter = SteamAPI_ISteamMatchmaking_AddRequestLobbyListNearValueFilter { { 0 } },
    AddRequestLobbyListFilterSlotsAvailable = SteamAPI_ISteamMatchmaking_AddRequestLobbyListFilterSlotsAvailable { { 0 } },
    AddRequestLobbyListDistanceFilter = SteamAPI_ISteamMatchmaking_AddRequestLobbyListDistanceFilter { { 0 } },
    AddRequestLobbyListResultCountFilter = SteamAPI_ISteamMatchmaking_AddRequestLobbyListResultCountFilter { { 0 } },
    AddRequestLobbyListCompatibleMembersFilter = SteamAPI_ISteamMatchmaking_AddRequestLobbyListCompatibleMembersFilter { { 0 } },
    GetLobbyByIndex = SteamAPI_ISteamMatchmaking_GetLobbyByIndex { { 0 } },
    CreateLobby = SteamAPI_ISteamMatchmaking_CreateLobby {
        {
            // a = lobby type, b = max members -> async call
            next_call()
        }
    },
    JoinLobby = SteamAPI_ISteamMatchmaking_JoinLobby { { next_call() } },
    LeaveLobby = SteamAPI_ISteamMatchmaking_LeaveLobby { { 0 } },
    InviteUserToLobby = SteamAPI_ISteamMatchmaking_InviteUserToLobby { { 0 } },
    GetNumLobbyMembers = SteamAPI_ISteamMatchmaking_GetNumLobbyMembers { { 0 } },
    GetLobbyMemberByIndex = SteamAPI_ISteamMatchmaking_GetLobbyMemberByIndex { { 0 } },
    GetLobbyData = SteamAPI_ISteamMatchmaking_GetLobbyData { { static_name(b"\0") } },
    SetLobbyData = SteamAPI_ISteamMatchmaking_SetLobbyData { { 0 } },
    GetLobbyDataCount = SteamAPI_ISteamMatchmaking_GetLobbyDataCount { { 0 } },
    GetLobbyDataByIndex = SteamAPI_ISteamMatchmaking_GetLobbyDataByIndex { { 0 } },
    DeleteLobbyData = SteamAPI_ISteamMatchmaking_DeleteLobbyData { { 0 } },
    GetLobbyMemberData = SteamAPI_ISteamMatchmaking_GetLobbyMemberData { { static_name(b"\0") } },
    SetLobbyMemberData = SteamAPI_ISteamMatchmaking_SetLobbyMemberData { { 0 } },
    SendLobbyChatMsg = SteamAPI_ISteamMatchmaking_SendLobbyChatMsg { { 0 } },
    GetLobbyChatEntry = SteamAPI_ISteamMatchmaking_GetLobbyChatEntry { { 0 } },
    RequestLobbyData = SteamAPI_ISteamMatchmaking_RequestLobbyData { { 0 } },
    SetLobbyGameServer = SteamAPI_ISteamMatchmaking_SetLobbyGameServer { { 0 } },
    GetLobbyGameServer = SteamAPI_ISteamMatchmaking_GetLobbyGameServer { { 0 } },
    SetLobbyMemberLimit = SteamAPI_ISteamMatchmaking_SetLobbyMemberLimit { { 0 } },
    GetLobbyMemberLimit = SteamAPI_ISteamMatchmaking_GetLobbyMemberLimit { { 0 } },
    SetLobbyType = SteamAPI_ISteamMatchmaking_SetLobbyType { { 0 } },
    SetLobbyJoinable = SteamAPI_ISteamMatchmaking_SetLobbyJoinable { { 0 } },
    GetLobbyOwner = SteamAPI_ISteamMatchmaking_GetLobbyOwner { { FAKE_STEAM_ID } },
    SetLobbyOwner = SteamAPI_ISteamMatchmaking_SetLobbyOwner { { 0 } },
    SetLinkedLobby = SteamAPI_ISteamMatchmaking_SetLinkedLobby { { 0 } },
}

// ---------------------------------------------------------------- ISteamMatchmakingServers

def_iface! { VMatchmakingServers, VMM_SERVERS, MMSERVERS_OBJ, ps, pa, pb, pc, pd, pe,
    RequestInternetServerList = SteamAPI_ISteamMatchmakingServers_RequestInternetServerList { { 0 } },
    RequestLANServerList = SteamAPI_ISteamMatchmakingServers_RequestLANServerList { { 0 } },
    RequestFriendsServerList = SteamAPI_ISteamMatchmakingServers_RequestFriendsServerList { { 0 } },
    RequestFavoritesServerList = SteamAPI_ISteamMatchmakingServers_RequestFavoritesServerList { { 0 } },
    RequestHistoryServerList = SteamAPI_ISteamMatchmakingServers_RequestHistoryServerList { { 0 } },
    RequestSpectatorServerList = SteamAPI_ISteamMatchmakingServers_RequestSpectatorServerList { { 0 } },
    ReleaseRequest = SteamAPI_ISteamMatchmakingServers_ReleaseRequest { { 0 } },
    GetServerDetails = SteamAPI_ISteamMatchmakingServers_GetServerDetails { { 0 } },
    CancelQuery = SteamAPI_ISteamMatchmakingServers_CancelQuery { { 0 } },
    RefreshQuery = SteamAPI_ISteamMatchmakingServers_RefreshQuery { { 0 } },
    IsRefreshing = SteamAPI_ISteamMatchmakingServers_IsRefreshing { { 0 } },
    GetServerCount = SteamAPI_ISteamMatchmakingServers_GetServerCount { { 0 } },
    RefreshServer = SteamAPI_ISteamMatchmakingServers_RefreshServer { { 0 } },
    PingServer = SteamAPI_ISteamMatchmakingServers_PingServer { { 0 } },
    PlayerDetails = SteamAPI_ISteamMatchmakingServers_PlayerDetails { { 0 } },
    ServerRules = SteamAPI_ISteamMatchmakingServers_ServerRules { { 0 } },
    CancelServerQuery = SteamAPI_ISteamMatchmakingServers_CancelServerQuery { { 0 } },
}

// ---------------------------------------------------------------- ISteamGameSearch

def_iface! { VGameSearch, VGAMESEARCH, GAMESEARCH_OBJ, ps, pa, pb, pc, pd, pe,
    AddGameSearchParams = SteamAPI_ISteamGameSearch_AddGameSearchParams { { 1 } },
    SearchForGameWithLobby = SteamAPI_ISteamGameSearch_SearchForGameWithLobby { { 1 } },
    SearchForGameSolo = SteamAPI_ISteamGameSearch_SearchForGameSolo { { 1 } },
    AcceptGame = SteamAPI_ISteamGameSearch_AcceptGame { { 1 } },
    DeclineGame = SteamAPI_ISteamGameSearch_DeclineGame { { 1 } },
    RetrieveConnectionDetails = SteamAPI_ISteamGameSearch_RetrieveConnectionDetails { { 1 } },
    EndGameSearch = SteamAPI_ISteamGameSearch_EndGameSearch { { 1 } },
    SetConnectionDetails = SteamAPI_ISteamGameSearch_SetConnectionDetails { { 1 } },
    SetGameHostParams = SteamAPI_ISteamGameSearch_SetGameHostParams { { 1 } },
    SubmitPlayerResult = SteamAPI_ISteamGameSearch_SubmitPlayerResult { { 1 } },
    EndGame = SteamAPI_ISteamGameSearch_EndGame { { 1 } },
    RequestPlayersForGame = SteamAPI_ISteamGameSearch_RequestPlayersForGame { { 1 } },
    HostConfirmGameStart = SteamAPI_ISteamGameSearch_HostConfirmGameStart { { 1 } },
    CancelRequestPlayersForGame = SteamAPI_ISteamGameSearch_CancelRequestPlayersForGame { { 1 } },
}

// ---------------------------------------------------------------- ISteamUserStats

def_iface! { VUserStats, VUSERSTATS, USERSTATS_OBJ, ps, pa, pb, pc, pd, pe,
    GetNumAchievements = SteamAPI_ISteamUserStats_GetNumAchievements { { 0 } },
    GetAchievement = SteamAPI_ISteamUserStats_GetAchievement { { 0 } },
    GetAchievementDisplayAttribute = SteamAPI_ISteamUserStats_GetAchievementDisplayAttribute { { static_name(b"\0") } },
    SetAchievement = SteamAPI_ISteamUserStats_SetAchievement { { 1 } },
    ClearAchievement = SteamAPI_ISteamUserStats_ClearAchievement { { 1 } },
    GetAchievementAchievedPercent = SteamAPI_ISteamUserStats_GetAchievementAchievedPercent { { 0 } },
    GetAchievementAndUnlockTime = SteamAPI_ISteamUserStats_GetAchievementAndUnlockTime { { 0 } },
    StoreStats = SteamAPI_ISteamUserStats_StoreStats { { 1 } },
    GetAchievementIcon = SteamAPI_ISteamUserStats_GetAchievementIcon { { 0 } },
    GetAchievementName = SteamAPI_ISteamUserStats_GetAchievementName { { static_name(b"\0") } },
    RequestCurrentStats = SteamAPI_ISteamUserStats_RequestCurrentStats { { 1 } },
    GetStatInt32 = SteamAPI_ISteamUserStats_GetStatInt32 {
        {
            if pb != 0 { *(pb as *mut i32) = 0; }
            1
        }
    },
    GetStatFloat = SteamAPI_ISteamUserStats_GetStatFloat {
        {
            if pb != 0 { *(pb as *mut f32) = 0.0; }
            1
        }
    },
    SetStatInt32 = SteamAPI_ISteamUserStats_SetStatInt32 { { 1 } },
    SetStatFloat = SteamAPI_ISteamUserStats_SetStatFloat { { 1 } },
    UpdateAvgRateStat = SteamAPI_ISteamUserStats_UpdateAvgRateStat { { 1 } },
    GetUserStatInt32 = SteamAPI_ISteamUserStats_GetUserStatInt32 {
        {
            if pc != 0 { *(pc as *mut i32) = 0; }
            1
        }
    },
    GetUserStatFloat = SteamAPI_ISteamUserStats_GetUserStatFloat {
        {
            if pc != 0 { *(pc as *mut f32) = 0.0; }
            1
        }
    },
    GetUserAchievement = SteamAPI_ISteamUserStats_GetUserAchievement { { 0 } },
    GetUserAchievementAndUnlockTime = SteamAPI_ISteamUserStats_GetUserAchievementAndUnlockTime { { 0 } },
    ResetAllStats = SteamAPI_ISteamUserStats_ResetAllStats { { 1 } },
    RequestUserStats = SteamAPI_ISteamUserStats_RequestUserStats { { next_call() } },
    FindOrCreateLeaderboard = SteamAPI_ISteamUserStats_FindOrCreateLeaderboard { { next_call() } },
    FindLeaderboard = SteamAPI_ISteamUserStats_FindLeaderboard { { next_call() } },
    GetLeaderboardName = SteamAPI_ISteamUserStats_GetLeaderboardName { { static_name(b"\0") } },
    GetLeaderboardEntryCount = SteamAPI_ISteamUserStats_GetLeaderboardEntryCount { { 0 } },
    GetLeaderboardSortMethod = SteamAPI_ISteamUserStats_GetLeaderboardSortMethod { { 0 } },
    GetLeaderboardDisplayType = SteamAPI_ISteamUserStats_GetLeaderboardDisplayType { { 0 } },
    DownloadLeaderboardEntries = SteamAPI_ISteamUserStats_DownloadLeaderboardEntries { { next_call() } },
    DownloadLeaderboardEntriesForUsers = SteamAPI_ISteamUserStats_DownloadLeaderboardEntriesForUsers { { next_call() } },
    GetDownloadedLeaderboardEntry = SteamAPI_ISteamUserStats_GetDownloadedLeaderboardEntry { { 0 } },
    UploadLeaderboardScore = SteamAPI_ISteamUserStats_UploadLeaderboardScore { { next_call() } },
    AttachLeaderboardUGC = SteamAPI_ISteamUserStats_AttachLeaderboardUGC { { next_call() } },
    GetNumberOfCurrentPlayers = SteamAPI_ISteamUserStats_GetNumberOfCurrentPlayers { { next_call() } },
    RequestGlobalAchievementPercentages = SteamAPI_ISteamUserStats_RequestGlobalAchievementPercentages { { next_call() } },
    GetMostAchievedAchievementInfo = SteamAPI_ISteamUserStats_GetMostAchievedAchievementInfo { { 0 } },
    GetNextMostAchievedAchievementInfo = SteamAPI_ISteamUserStats_GetNextMostAchievedAchievementInfo { { -1i64 as u64 } },
    GetGlobalStatInt64 = SteamAPI_ISteamUserStats_GetGlobalStatInt64 {
        {
            if pb != 0 { *(pb as *mut i64) = 0; }
            1
        }
    },
    GetGlobalStatDouble = SteamAPI_ISteamUserStats_GetGlobalStatDouble {
        {
            if pb != 0 { *(pb as *mut f64) = 0.0; }
            1
        }
    },
    GetGlobalStatHistoryInt64 = SteamAPI_ISteamUserStats_GetGlobalStatHistoryInt64 {
        {
            if pc != 0 { *(pc as *mut i64) = 0; }
            0
        }
    },
    GetGlobalStatHistoryDouble = SteamAPI_ISteamUserStats_GetGlobalStatHistoryDouble {
        {
            if pc != 0 { *(pc as *mut f64) = 0.0; }
            0
        }
    },
    RequestGlobalStats = SteamAPI_ISteamUserStats_RequestGlobalStats { { next_call() } },
    IndicateAchievementProgress = SteamAPI_ISteamUserStats_IndicateAchievementProgress { { 1 } },
}

// ---------------------------------------------------------------- ISteamNetworking

def_iface! { VNetworking, VNETWORKING, NETWORKING_OBJ, ps, pa, pb, pc, pd, pe,
    SendP2PPacket = SteamAPI_ISteamNetworking_SendP2PPacket { { 1 } },
    IsP2PPacketAvailable = SteamAPI_ISteamNetworking_IsP2PPacketAvailable {
        {
            if pb != 0 { *(pb as *mut u32) = 0; }
            0
        }
    },
    ReadP2PPacket = SteamAPI_ISteamNetworking_ReadP2PPacket {
        {
            if pd != 0 { *(pd as *mut u32) = 0; }
            0
        }
    },
    AcceptP2PSessionWithUser = SteamAPI_ISteamNetworking_AcceptP2PSessionWithUser { { 1 } },
    CloseP2PSessionWithUser = SteamAPI_ISteamNetworking_CloseP2PSessionWithUser { { 1 } },
    CloseP2PChannelWithUser = SteamAPI_ISteamNetworking_CloseP2PChannelWithUser { { 1 } },
    GetP2PSessionState = SteamAPI_ISteamNetworking_GetP2PSessionState {
        {
            // a = steamID, b = out state struct -> report connected relay
            if pb != 0 {
                let p = pb as *mut u8;
                std::ptr::write_bytes(p, 0, 32);
                std::ptr::write(p as *mut u8, 1u8); // m_bConnectionActive
                std::ptr::write(p.add(1) as *mut u8, 1u8); // m_bConnecting (legacy layout)
            }
            1
        }
    },
    AllowP2PPacketRelay = SteamAPI_ISteamNetworking_AllowP2PPacketRelay { { 0 } },
    CreateListenSocket = SteamAPI_ISteamNetworking_CreateListenSocket { { 1 } },
    CreateP2PConnectionSocket = SteamAPI_ISteamNetworking_CreateP2PConnectionSocket { { 2 } },
    CreateConnectionSocket = SteamAPI_ISteamNetworking_CreateConnectionSocket { { 3 } },
    DestroySocket = SteamAPI_ISteamNetworking_DestroySocket { { 1 } },
    DestroyListenSocket = SteamAPI_ISteamNetworking_DestroyListenSocket { { 1 } },
    SendDataOnSocket = SteamAPI_ISteamNetworking_SendDataOnSocket { { 1 } },
    IsDataAvailable = SteamAPI_ISteamNetworking_IsDataAvailable { { 0 } },
    RetrieveData = SteamAPI_ISteamNetworking_RetrieveData {
        {
            if pc != 0 { *(pc as *mut u32) = 0; }
            0
        }
    },
    IsDataAvailableOnSocket = SteamAPI_ISteamNetworking_IsDataAvailableOnSocket { { 0 } },
    RetrieveDataFromSocket = SteamAPI_ISteamNetworking_RetrieveDataFromSocket {
        {
            if pd != 0 { *(pd as *mut u32) = 0; }
            0
        }
    },
    GetSocketInfo = SteamAPI_ISteamNetworking_GetSocketInfo { { 0 } },
    GetListenSocketInfo = SteamAPI_ISteamNetworking_GetListenSocketInfo { { 0 } },
    GetSocketConnectionType = SteamAPI_ISteamNetworking_GetSocketConnectionType { { 0 } },
    GetMaxPacketSize = SteamAPI_ISteamNetworking_GetMaxPacketSize { { 1200 } },
}

// ---------------------------------------------------------------- ISteamRemoteStorage

def_iface! { VRemoteStorage, VREMOTESTORAGE, REMOTESTORAGE_OBJ, ps, pa, pb, pc, pd, pe,
    FileWrite = SteamAPI_ISteamRemoteStorage_FileWrite { { 1 } },
    FileRead = SteamAPI_ISteamRemoteStorage_FileRead {
        {
            // a = name, b = buffer, c = max -> 0 bytes (no data)
            0
        }
    },
    FileWriteAsync = SteamAPI_ISteamRemoteStorage_FileWriteAsync { { next_call() } },
    FileReadAsync = SteamAPI_ISteamRemoteStorage_FileReadAsync { { next_call() } },
    FileReadAsyncComplete = SteamAPI_ISteamRemoteStorage_FileReadAsyncComplete {
        {
            if pc != 0 { *(pc as *mut u32) = 0; }
            0
        }
    },
    FileForget = SteamAPI_ISteamRemoteStorage_FileForget { { 1 } },
    FileDelete = SteamAPI_ISteamRemoteStorage_FileDelete { { 1 } },
    FileShare = SteamAPI_ISteamRemoteStorage_FileShare { { next_call() } },
    SetCloudEnabledForApp = SteamAPI_ISteamRemoteStorage_SetCloudEnabledForApp { { 0 } },
    IsCloudEnabledForApp = SteamAPI_ISteamRemoteStorage_IsCloudEnabledForApp { { 1 } },
    FileExists = SteamAPI_ISteamRemoteStorage_FileExists { { 0 } },
    FilePersisted = SteamAPI_ISteamRemoteStorage_FilePersisted { { 0 } },
    GetFileSize = SteamAPI_ISteamRemoteStorage_GetFileSize { { 0 } },
    GetFileTimestamp = SteamAPI_ISteamRemoteStorage_GetFileTimestamp { { 0 } },
    GetSyncPlatforms = SteamAPI_ISteamRemoteStorage_GetSyncPlatforms { { 3 } },
    SetSyncPlatforms = SteamAPI_ISteamRemoteStorage_SetSyncPlatforms { { 1 } },
    GetFileCount = SteamAPI_ISteamRemoteStorage_GetFileCount { { 0 } },
    GetFileNameAndSize = SteamAPI_ISteamRemoteStorage_GetFileNameAndSize { { static_name(b"\0") } },
    GetQuota = SteamAPI_ISteamRemoteStorage_GetQuota {
        {
            if pa != 0 { *(pa as *mut u64) = 20 * 1024 * 1024; }
            if pb != 0 { *(pb as *mut u64) = 0; }
            1
        }
    },
    IsCloudEnabledForAccount = SteamAPI_ISteamRemoteStorage_IsCloudEnabledForAccount { { 1 } },
    FileWriteStreamOpen = SteamAPI_ISteamRemoteStorage_FileWriteStreamOpen { { 1 } },
    FileWriteStreamWriteChunk = SteamAPI_ISteamRemoteStorage_FileWriteStreamWriteChunk { { 1 } },
    FileWriteStreamClose = SteamAPI_ISteamRemoteStorage_FileWriteStreamClose { { 1 } },
    FileWriteStreamCancel = SteamAPI_ISteamRemoteStorage_FileWriteStreamCancel { { 1 } },
    PublishWorkshopFile = SteamAPI_ISteamRemoteStorage_PublishWorkshopFile { { next_call() } },
    CreatePublishedFileUpdateRequest = SteamAPI_ISteamRemoteStorage_CreatePublishedFileUpdateRequest { { 1 } },
    UpdatePublishedFileFile = SteamAPI_ISteamRemoteStorage_UpdatePublishedFileFile { { 1 } },
    UpdatePublishedFilePreviewFile = SteamAPI_ISteamRemoteStorage_UpdatePublishedFilePreviewFile { { 1 } },
    UpdatePublishedFileTitle = SteamAPI_ISteamRemoteStorage_UpdatePublishedFileTitle { { 1 } },
    UpdatePublishedFileDescription = SteamAPI_ISteamRemoteStorage_UpdatePublishedFileDescription { { 1 } },
    UpdatePublishedFileSetChangeDescription = SteamAPI_ISteamRemoteStorage_UpdatePublishedFileSetChangeDescription { { 1 } },
    UpdatePublishedFileVisibility = SteamAPI_ISteamRemoteStorage_UpdatePublishedFileVisibility { { 1 } },
    UpdatePublishedFileTags = SteamAPI_ISteamRemoteStorage_UpdatePublishedFileTags { { 1 } },
    CommitPublishedFileUpdate = SteamAPI_ISteamRemoteStorage_CommitPublishedFileUpdate { { next_call() } },
    GetPublishedFileDetails = SteamAPI_ISteamRemoteStorage_GetPublishedFileDetails { { next_call() } },
    DeletePublishedFile = SteamAPI_ISteamRemoteStorage_DeletePublishedFile { { next_call() } },
    EnumerateUserPublishedFiles = SteamAPI_ISteamRemoteStorage_EnumerateUserPublishedFiles { { next_call() } },
    SubscribePublishedFile = SteamAPI_ISteamRemoteStorage_SubscribePublishedFile { { next_call() } },
    EnumerateUserSubscribedFiles = SteamAPI_ISteamRemoteStorage_EnumerateUserSubscribedFiles { { next_call() } },
    UnsubscribePublishedFile = SteamAPI_ISteamRemoteStorage_UnsubscribePublishedFile { { next_call() } },
    UpdateUserPublishedItemVote = SteamAPI_ISteamRemoteStorage_UpdateUserPublishedItemVote { { next_call() } },
    GetUserPublishedItemVoteDetails = SteamAPI_ISteamRemoteStorage_GetUserPublishedItemVoteDetails { { next_call() } },
    EnumerateUserSharedWorkshopFiles = SteamAPI_ISteamRemoteStorage_EnumerateUserSharedWorkshopFiles { { next_call() } },
    PublishVideo = SteamAPI_ISteamRemoteStorage_PublishVideo { { next_call() } },
    SetUserPublishedFileAction = SteamAPI_ISteamRemoteStorage_SetUserPublishedFileAction { { next_call() } },
    EnumeratePublishedFilesByUserAction = SteamAPI_ISteamRemoteStorage_EnumeratePublishedFilesByUserAction { { next_call() } },
    EnumeratePublishedWorkshopFiles = SteamAPI_ISteamRemoteStorage_EnumeratePublishedWorkshopFiles { { next_call() } },
    UGCDownloadToLocation = SteamAPI_ISteamRemoteStorage_UGCDownloadToLocation { { next_call() } },
    UGCDownload = SteamAPI_ISteamRemoteStorage_UGCDownload { { next_call() } },
    UGCRead = SteamAPI_ISteamRemoteStorage_UGCRead { { 0 } },
    GetUGCDownloadProgress = SteamAPI_ISteamRemoteStorage_GetUGCDownloadProgress {
        {
            if pb != 0 { *(pb as *mut u64) = 0; }
            if pc != 0 { *(pc as *mut u64) = 0; }
            1
        }
    },
    GetUGCDetails = SteamAPI_ISteamRemoteStorage_GetUGCDetails { { 0 } },
    GetCachedUGCCount = SteamAPI_ISteamRemoteStorage_GetCachedUGCCount { { 0 } },
    GetCachedUGCHandle = SteamAPI_ISteamRemoteStorage_GetCachedUGCHandle { { 0xFFFFFFFFFFFFFFFF } },
    GetPublishedItemVoteDetails = SteamAPI_ISteamRemoteStorage_GetPublishedItemVoteDetails { { next_call() } },
}

// ---------------------------------------------------------------- ISteamScreenshots

def_iface! { VScreenshots, VSCREENSHOTS, SCREENSHOTS_OBJ, ps, pa, pb, pc, pd, pe,
    WriteScreenshot = SteamAPI_ISteamScreenshots_WriteScreenshot { { next_call() } },
    AddScreenshotToLibrary = SteamAPI_ISteamScreenshots_AddScreenshotToLibrary { { next_call() } },
    TriggerScreenshot = SteamAPI_ISteamScreenshots_TriggerScreenshot { { 0 } },
    HookScreenshots = SteamAPI_ISteamScreenshots_HookScreenshots { { 0 } },
    SetLocation = SteamAPI_ISteamScreenshots_SetLocation { { 0 } },
    TagUser = SteamAPI_ISteamScreenshots_TagUser { { 0 } },
    TagPublishedFile = SteamAPI_ISteamScreenshots_TagPublishedFile { { 0 } },
    IsScreenshotsHooked = SteamAPI_ISteamScreenshots_IsScreenshotsHooked { { 0 } },
    AddVRScreenshotToLibrary = SteamAPI_ISteamScreenshots_AddVRScreenshotToLibrary { { next_call() } },
}

// ---------------------------------------------------------------- ISteamHTTP

def_iface! { VHTTP, VHTTP, HTTP_OBJ, ps, pa, pb, pc, pd, pe,
    CreateHTTPRequest = SteamAPI_ISteamHTTP_CreateHTTPRequest { { next_call() as u32 as u64 } },
    SetHTTPRequestContextValue = SteamAPI_ISteamHTTP_SetHTTPRequestContextValue { { 1 } },
    SetHTTPRequestNetworkActivityTimeout = SteamAPI_ISteamHTTP_SetHTTPRequestNetworkActivityTimeout { { 1 } },
    SetHTTPRequestHeaderValue = SteamAPI_ISteamHTTP_SetHTTPRequestHeaderValue { { 1 } },
    SetHTTPRequestGetOrPostParameter = SteamAPI_ISteamHTTP_SetHTTPRequestGetOrPostParameter { { 1 } },
    SendHTTPRequest = SteamAPI_ISteamHTTP_SendHTTPRequest { { 1 } },
    SendHTTPRequestAndStreamResponse = SteamAPI_ISteamHTTP_SendHTTPRequestAndStreamResponse { { 1 } },
    DeferHTTPRequest = SteamAPI_ISteamHTTP_DeferHTTPRequest { { 1 } },
    PrioritizeHTTPRequest = SteamAPI_ISteamHTTP_PrioritizeHTTPRequest { { 1 } },
    GetHTTPResponseHeaderSize = SteamAPI_ISteamHTTP_GetHTTPResponseHeaderSize { { 0 } },
    GetHTTPResponseHeaderValue = SteamAPI_ISteamHTTP_GetHTTPResponseHeaderValue { { 0 } },
    GetHTTPResponseBodySize = SteamAPI_ISteamHTTP_GetHTTPResponseBodySize {
        {
            if pb != 0 { *(pb as *mut u32) = 0; }
            1
        }
    },
    GetHTTPResponseBodyData = SteamAPI_ISteamHTTP_GetHTTPResponseBodyData { { 0 } },
    GetHTTPStreamingResponseBodyData = SteamAPI_ISteamHTTP_GetHTTPStreamingResponseBodyData { { 0 } },
    ReleaseHTTPRequest = SteamAPI_ISteamHTTP_ReleaseHTTPRequest { { 1 } },
    GetHTTPDownloadProgressPct = SteamAPI_ISteamHTTP_GetHTTPDownloadProgressPct {
        {
            if pb != 0 { *(pb as *mut f32) = 0.0; }
            1
        }
    },
    SetHTTPRequestRawPostBody = SteamAPI_ISteamHTTP_SetHTTPRequestRawPostBody { { 1 } },
    CreateCookieContainer = SteamAPI_ISteamHTTP_CreateCookieContainer { { 1 } },
    ReleaseCookieContainer = SteamAPI_ISteamHTTP_ReleaseCookieContainer { { 1 } },
    SetCookie = SteamAPI_ISteamHTTP_SetCookie { { 1 } },
    SetHTTPRequestUserAgentInfo = SteamAPI_ISteamHTTP_SetHTTPRequestUserAgentInfo { { 1 } },
    SetHTTPRequestRequiresVerifiedCertificate = SteamAPI_ISteamHTTP_SetHTTPRequestRequiresVerifiedCertificate { { 1 } },
    SetHTTPRequestAbsoluteTimeoutMS = SteamAPI_ISteamHTTP_SetHTTPRequestAbsoluteTimeoutMS { { 1 } },
    GetHTTPRequestWasTimedOut = SteamAPI_ISteamHTTP_GetHTTPRequestWasTimedOut { { 0 } },
    SetHTTPRequestCookieContainer = SteamAPI_ISteamHTTP_SetHTTPRequestCookieContainer { { 1 } },
}

// ---------------------------------------------------------------- ISteamInput

def_iface! { VInput, VINPUT, INPUT_OBJ, ps, pa, pb, pc, pd, pe,
    Init = SteamAPI_ISteamInput_Init { { 1 } },
    Shutdown = SteamAPI_ISteamInput_Shutdown { { 1 } },
    RunFrame = SteamAPI_ISteamInput_RunFrame { { 0 } },
    GetConnectedControllers = SteamAPI_ISteamInput_GetConnectedControllers {
        {
            if pa != 0 { *(pa as *mut u64) = 0; }
            0
        }
    },
    ShowBindingPanel = SteamAPI_ISteamInput_ShowBindingPanel { { 0 } },
    GetActionSetHandle = SteamAPI_ISteamInput_GetActionSetHandle { { 1 } },
    ActivateActionSet = SteamAPI_ISteamInput_ActivateActionSet { { 0 } },
    GetCurrentActionSet = SteamAPI_ISteamInput_GetCurrentActionSet { { 1 } },
    ActivateActionSetLayer = SteamAPI_ISteamInput_ActivateActionSetLayer { { 0 } },
    DeactivateActionSetLayer = SteamAPI_ISteamInput_DeactivateActionSetLayer { { 0 } },
    DeactivateAllActionSetLayers = SteamAPI_ISteamInput_DeactivateAllActionSetLayers { { 0 } },
    GetActiveActionSetLayers = SteamAPI_ISteamInput_GetActiveActionSetLayers { { 0 } },
    GetDigitalActionHandle = SteamAPI_ISteamInput_GetDigitalActionHandle { { 1 } },
    GetDigitalActionData = SteamAPI_ISteamInput_GetDigitalActionData { { 0 } },
    GetDigitalActionOrigins = SteamAPI_ISteamInput_GetDigitalActionOrigins { { 0 } },
    GetAnalogActionHandle = SteamAPI_ISteamInput_GetAnalogActionHandle { { 1 } },
    GetAnalogActionData = SteamAPI_ISteamInput_GetAnalogActionData { { 0 } },
    GetAnalogActionOrigins = SteamAPI_ISteamInput_GetAnalogActionOrigins { { 0 } },
    GetMotionData = SteamAPI_ISteamInput_GetMotionData { { 0 } },
    TriggerVibration = SteamAPI_ISteamInput_TriggerVibration { { 0 } },
    SetLEDColor = SteamAPI_ISteamInput_SetLEDColor { { 0 } },
    TriggerHapticPulse = SteamAPI_ISteamInput_TriggerHapticPulse { { 0 } },
    TriggerRepeatedHapticPulse = SteamAPI_ISteamInput_TriggerRepeatedHapticPulse { { 0 } },
    GetGamepadIndexForController = SteamAPI_ISteamInput_GetGamepadIndexForController { { -1i64 as u64 } },
    GetControllerForGamepadIndex = SteamAPI_ISteamInput_GetControllerForGamepadIndex { { 0 } },
    GetInputTypeForHandle = SteamAPI_ISteamInput_GetInputTypeForHandle { { 0 } },
    GetRemotePlaySessionID = SteamAPI_ISteamInput_GetRemotePlaySessionID { { 0 } },
    GetActionOriginFromXboxOrigin = SteamAPI_ISteamInput_GetActionOriginFromXboxOrigin { { 0 } },
    GetDeviceBindingRevision = SteamAPI_ISteamInput_GetDeviceBindingRevision { { 1 } },
    GetGlyphForActionOrigin = SteamAPI_ISteamInput_GetGlyphForActionOrigin { { static_name(b"\0") } },
    GetGlyphForXboxOrigin = SteamAPI_ISteamInput_GetGlyphForXboxOrigin { { static_name(b"\0") } },
    GetStringForActionOrigin = SteamAPI_ISteamInput_GetStringForActionOrigin { { static_name(b"\0") } },
    GetStringForXboxOrigin = SteamAPI_ISteamInput_GetStringForXboxOrigin { { static_name(b"\0") } },
    StopAnalogActionMomentum = SteamAPI_ISteamInput_StopAnalogActionMomentum { { 0 } },
    TranslateActionOrigin = SteamAPI_ISteamInput_TranslateActionOrigin { { 0 } },
}

// ---------------------------------------------------------------- ISteamController (legacy)

def_iface! { VController, VCONTROLLER, CONTROLLER_OBJ, ps, pa, pb, pc, pd, pe,
    Init = SteamAPI_ISteamController_Init { { 1 } },
    Shutdown = SteamAPI_ISteamController_Shutdown { { 1 } },
    RunFrame = SteamAPI_ISteamController_RunFrame { { 0 } },
    GetConnectedControllers = SteamAPI_ISteamController_GetConnectedControllers {
        {
            if pa != 0 { *(pa as *mut u64) = 0; }
            0
        }
    },
    ShowBindingPanel = SteamAPI_ISteamController_ShowBindingPanel { { 0 } },
    GetActionSetHandle = SteamAPI_ISteamController_GetActionSetHandle { { 1 } },
    ActivateActionSet = SteamAPI_ISteamController_ActivateActionSet { { 0 } },
    GetCurrentActionSet = SteamAPI_ISteamController_GetCurrentActionSet { { 1 } },
    ActivateActionSetLayer = SteamAPI_ISteamController_ActivateActionSetLayer { { 0 } },
    DeactivateActionSetLayer = SteamAPI_ISteamController_DeactivateActionSetLayer { { 0 } },
    DeactivateAllActionSetLayers = SteamAPI_ISteamController_DeactivateAllActionSetLayers { { 0 } },
    GetActiveActionSetLayers = SteamAPI_ISteamController_GetActiveActionSetLayers { { 0 } },
    GetDigitalActionHandle = SteamAPI_ISteamController_GetDigitalActionHandle { { 1 } },
    GetDigitalActionData = SteamAPI_ISteamController_GetDigitalActionData { { 0 } },
    GetDigitalActionOrigins = SteamAPI_ISteamController_GetDigitalActionOrigins { { 0 } },
    GetAnalogActionHandle = SteamAPI_ISteamController_GetAnalogActionHandle { { 1 } },
    GetAnalogActionData = SteamAPI_ISteamController_GetAnalogActionData { { 0 } },
    GetAnalogActionOrigins = SteamAPI_ISteamController_GetAnalogActionOrigins { { 0 } },
    GetMotionData = SteamAPI_ISteamController_GetMotionData { { 0 } },
    TriggerHapticPulse = SteamAPI_ISteamController_TriggerHapticPulse { { 0 } },
    TriggerRepeatedHapticPulse = SteamAPI_ISteamController_TriggerRepeatedHapticPulse { { 0 } },
    TriggerVibration = SteamAPI_ISteamController_TriggerVibration { { 0 } },
    SetLEDColor = SteamAPI_ISteamController_SetLEDColor { { 0 } },
    GetGamepadIndexForController = SteamAPI_ISteamController_GetGamepadIndexForController { { -1i64 as u64 } },
    GetControllerForGamepadIndex = SteamAPI_ISteamController_GetControllerForGamepadIndex { { 0 } },
    GetInputTypeForHandle = SteamAPI_ISteamController_GetInputTypeForHandle { { 0 } },
    GetControllerBindingRevision = SteamAPI_ISteamController_GetControllerBindingRevision { { 1 } },
    GetActionOriginFromXboxOrigin = SteamAPI_ISteamController_GetActionOriginFromXboxOrigin { { 0 } },
    GetGlyphForActionOrigin = SteamAPI_ISteamController_GetGlyphForActionOrigin { { static_name(b"\0") } },
    GetGlyphForXboxOrigin = SteamAPI_ISteamController_GetGlyphForXboxOrigin { { static_name(b"\0") } },
    GetStringForActionOrigin = SteamAPI_ISteamController_GetStringForActionOrigin { { static_name(b"\0") } },
    GetStringForXboxOrigin = SteamAPI_ISteamController_GetStringForXboxOrigin { { static_name(b"\0") } },
    StopAnalogActionMomentum = SteamAPI_ISteamController_StopAnalogActionMomentum { { 0 } },
    TranslateActionOrigin = SteamAPI_ISteamController_TranslateActionOrigin { { 0 } },
}

// ---------------------------------------------------------------- ISteamUGC

def_iface! { VUgc, VUGC, UGC_OBJ, ps, pa, pb, pc, pd, pe,
    CreateQueryUGCDetailsRequest = SteamAPI_ISteamUGC_CreateQueryUGCDetailsRequest { { next_call() } },
    CreateQueryAllUGCRequestPage = SteamAPI_ISteamUGC_CreateQueryAllUGCRequestPage { { next_call() } },
    CreateQueryAllUGCRequestCursor = SteamAPI_ISteamUGC_CreateQueryAllUGCRequestCursor { { next_call() } },
    CreateQueryUserUGCRequest = SteamAPI_ISteamUGC_CreateQueryUserUGCRequest { { next_call() } },
    SetCloudFileNameFilter = SteamAPI_ISteamUGC_SetCloudFileNameFilter { { 1 } },
    SetMatchAnyTag = SteamAPI_ISteamUGC_SetMatchAnyTag { { 1 } },
    SetSearchText = SteamAPI_ISteamUGC_SetSearchText { { 1 } },
    SetRankedByTrendDays = SteamAPI_ISteamUGC_SetRankedByTrendDays { { 1 } },
    AddRequiredTag = SteamAPI_ISteamUGC_AddRequiredTag { { 1 } },
    AddExcludedTag = SteamAPI_ISteamUGC_AddExcludedTag { { 1 } },
    SetReturnOnlyIDs = SteamAPI_ISteamUGC_SetReturnOnlyIDs { { 1 } },
    SetReturnKeyValueTags = SteamAPI_ISteamUGC_SetReturnKeyValueTags { { 1 } },
    SetReturnLongDescription = SteamAPI_ISteamUGC_SetReturnLongDescription { { 1 } },
    SetReturnMetadata = SteamAPI_ISteamUGC_SetReturnMetadata { { 1 } },
    SetReturnChildren = SteamAPI_ISteamUGC_SetReturnChildren { { 1 } },
    SetReturnAdditionalPreviews = SteamAPI_ISteamUGC_SetReturnAdditionalPreviews { { 1 } },
    SetReturnTotalOnly = SteamAPI_ISteamUGC_SetReturnTotalOnly { { 1 } },
    SetReturnPlaytimeStats = SteamAPI_ISteamUGC_SetReturnPlaytimeStats { { 1 } },
    SetLanguage = SteamAPI_ISteamUGC_SetLanguage { { 1 } },
    SetAllowCachedResponse = SteamAPI_ISteamUGC_SetAllowCachedResponse { { 1 } },
    GetQueryUGCResult = SteamAPI_ISteamUGC_GetQueryUGCResult { { 0 } },
    GetQueryUGCPreviewURL = SteamAPI_ISteamUGC_GetQueryUGCPreviewURL { { 0 } },
    GetQueryUGCMetadata = SteamAPI_ISteamUGC_GetQueryUGCMetadata { { 0 } },
    GetQueryUGCChildren = SteamAPI_ISteamUGC_GetQueryUGCChildren { { 0 } },
    GetQueryUGCStatistic = SteamAPI_ISteamUGC_GetQueryUGCStatistic { { 0 } },
    GetQueryUGCNumAdditionalPreviews = SteamAPI_ISteamUGC_GetQueryUGCNumAdditionalPreviews { { 0 } },
    GetQueryUGCAdditionalPreview = SteamAPI_ISteamUGC_GetQueryUGCAdditionalPreview { { 0 } },
    GetQueryUGCNumKeyValueTags = SteamAPI_ISteamUGC_GetQueryUGCNumKeyValueTags { { 0 } },
    GetQueryUGCKeyValueTag = SteamAPI_ISteamUGC_GetQueryUGCKeyValueTag { { 0 } },
    GetQueryFirstUGCKeyValueTag = SteamAPI_ISteamUGC_GetQueryFirstUGCKeyValueTag { { 0 } },
    ReleaseQueryUGCRequest = SteamAPI_ISteamUGC_ReleaseQueryUGCRequest { { 1 } },
    AddRequiredKeyValueTag = SteamAPI_ISteamUGC_AddRequiredKeyValueTag { { 1 } },
    RequestUGCDetails = SteamAPI_ISteamUGC_RequestUGCDetails { { next_call() } },
    CreateItem = SteamAPI_ISteamUGC_CreateItem { { next_call() } },
    StartItemUpdate = SteamAPI_ISteamUGC_StartItemUpdate { { next_call() } },
    SetItemTitle = SteamAPI_ISteamUGC_SetItemTitle { { 1 } },
    SetItemDescription = SteamAPI_ISteamUGC_SetItemDescription { { 1 } },
    SetItemUpdateLanguage = SteamAPI_ISteamUGC_SetItemUpdateLanguage { { 1 } },
    SetItemMetadata = SteamAPI_ISteamUGC_SetItemMetadata { { 1 } },
    SetItemVisibility = SteamAPI_ISteamUGC_SetItemVisibility { { 1 } },
    SetItemTags = SteamAPI_ISteamUGC_SetItemTags { { 1 } },
    SetItemContent = SteamAPI_ISteamUGC_SetItemContent { { 1 } },
    SetItemPreview = SteamAPI_ISteamUGC_SetItemPreview { { 1 } },
    SetAllowLegacyUpload = SteamAPI_ISteamUGC_SetAllowLegacyUpload { { 1 } },
    RemoveItemKeyValueTags = SteamAPI_ISteamUGC_RemoveItemKeyValueTags { { 1 } },
    RemoveAllItemKeyValueTags = SteamAPI_ISteamUGC_RemoveAllItemKeyValueTags { { 1 } },
    AddItemKeyValueTag = SteamAPI_ISteamUGC_AddItemKeyValueTag { { 1 } },
    AddItemPreviewFile = SteamAPI_ISteamUGC_AddItemPreviewFile { { 1 } },
    AddItemPreviewVideo = SteamAPI_ISteamUGC_AddItemPreviewVideo { { 1 } },
    UpdateItemPreviewFile = SteamAPI_ISteamUGC_UpdateItemPreviewFile { { 1 } },
    UpdateItemPreviewVideo = SteamAPI_ISteamUGC_UpdateItemPreviewVideo { { 1 } },
    RemoveItemPreview = SteamAPI_ISteamUGC_RemoveItemPreview { { 1 } },
    SubmitItemUpdate = SteamAPI_ISteamUGC_SubmitItemUpdate { { next_call() } },
    GetItemUpdateProgress = SteamAPI_ISteamUGC_GetItemUpdateProgress { { 0 } },
    SetUserItemVote = SteamAPI_ISteamUGC_SetUserItemVote { { next_call() } },
    GetUserItemVote = SteamAPI_ISteamUGC_GetUserItemVote { { next_call() } },
    AddItemToFavorites = SteamAPI_ISteamUGC_AddItemToFavorites { { next_call() } },
    RemoveItemFromFavorites = SteamAPI_ISteamUGC_RemoveItemFromFavorites { { next_call() } },
    SubscribeItem = SteamAPI_ISteamUGC_SubscribeItem { { next_call() } },
    UnsubscribeItem = SteamAPI_ISteamUGC_UnsubscribeItem { { next_call() } },
    GetNumSubscribedItems = SteamAPI_ISteamUGC_GetNumSubscribedItems { { 0 } },
    GetSubscribedItems = SteamAPI_ISteamUGC_GetSubscribedItems { { 0 } },
    GetItemState = SteamAPI_ISteamUGC_GetItemState { { 0 } },
    GetItemInstallInfo = SteamAPI_ISteamUGC_GetItemInstallInfo { { 0 } },
    GetItemDownloadInfo = SteamAPI_ISteamUGC_GetItemDownloadInfo { { 0 } },
    DownloadItem = SteamAPI_ISteamUGC_DownloadItem { { 0 } },
    BInitWorkshopForGameServer = SteamAPI_ISteamUGC_BInitWorkshopForGameServer { { 1 } },
    SuspendDownloads = SteamAPI_ISteamUGC_SuspendDownloads { { 0 } },
    StartPlaytimeTracking = SteamAPI_ISteamUGC_StartPlaytimeTracking { { next_call() } },
    StopPlaytimeTracking = SteamAPI_ISteamUGC_StopPlaytimeTracking { { next_call() } },
    StopPlaytimeTrackingForAllItems = SteamAPI_ISteamUGC_StopPlaytimeTrackingForAllItems { { next_call() } },
    AddDependency = SteamAPI_ISteamUGC_AddDependency { { next_call() } },
    RemoveDependency = SteamAPI_ISteamUGC_RemoveDependency { { next_call() } },
    AddAppDependency = SteamAPI_ISteamUGC_AddAppDependency { { next_call() } },
    RemoveAppDependency = SteamAPI_ISteamUGC_RemoveAppDependency { { next_call() } },
    GetAppDependencies = SteamAPI_ISteamUGC_GetAppDependencies { { next_call() } },
    DeleteItem = SteamAPI_ISteamUGC_DeleteItem { { next_call() } },
    SendQueryUGCRequest = SteamAPI_ISteamUGC_SendQueryUGCRequest { { next_call() } },
    AddRequiredTagGroup = SteamAPI_ISteamUGC_AddRequiredTagGroup { { 1 } },
}

// ---------------------------------------------------------------- ISteamAppList

def_iface! { VAppList, VAPPLIST, APPLIST_OBJ, ps, pa, pb, pc, pd, pe,
    GetNumInstalledApps = SteamAPI_ISteamAppList_GetNumInstalledApps { { 1 } },
    GetInstalledApps = SteamAPI_ISteamAppList_GetInstalledApps {
        {
            // a = out array, b = max -> one app id
            if pa != 0 && pb >= 1 {
                *(pa as *mut u32) = FAKE_APP_ID;
                return 1;
            }
            1
        }
    },
    GetAppName = SteamAPI_ISteamAppList_GetAppName {
        {
            // a = app id, b = out buffer, c = size -> "Rec Room"
            if pb != 0 && pc > 8 {
                let s = b"Rec Room\0";
                let n = s.len().min(pc as usize);
                std::ptr::copy_nonoverlapping(s.as_ptr(), pb as *mut u8, n);
                return 0;
            }
            -1i64 as u64
        }
    },
    GetAppInstallDir = SteamAPI_ISteamAppList_GetAppInstallDir {
        {
            if pb != 0 && pc > 8 {
                let s = b"FluxRec\\game\0";
                let n = s.len().min(pc as usize);
                std::ptr::copy_nonoverlapping(s.as_ptr(), pb as *mut u8, n);
                return 0;
            }
            -1i64 as u64
        }
    },
    GetAppBuildId = SteamAPI_ISteamAppList_GetAppBuildId { { BUILD_ID as u64 } },
}

// ---------------------------------------------------------------- ISteamMusic

def_iface! { VMusic, VMUSIC, MUSIC_OBJ, ps, pa, pb, pc, pd, pe,
    BIsEnabled = SteamAPI_ISteamMusic_BIsEnabled { { 0 } },
    BIsPlaying = SteamAPI_ISteamMusic_BIsPlaying { { 0 } },
    GetPlaybackStatus = SteamAPI_ISteamMusic_GetPlaybackStatus { { 0 } },
    Play = SteamAPI_ISteamMusic_Play { { 0 } },
    Pause = SteamAPI_ISteamMusic_Pause { { 0 } },
    PlayPrevious = SteamAPI_ISteamMusic_PlayPrevious { { 0 } },
    PlayNext = SteamAPI_ISteamMusic_PlayNext { { 0 } },
    SetVolume = SteamAPI_ISteamMusic_SetVolume { { 0 } },
    GetVolume = SteamAPI_ISteamMusic_GetVolume { { 1.0f32.to_bits() as u64 } },
}

// ---------------------------------------------------------------- ISteamMusicRemote

def_iface! { VMusicRemote, VMUSICREMOTE, MUSICREMOTE_OBJ, ps, pa, pb, pc, pd, pe,
    RegisterSteamMusicRemote = SteamAPI_ISteamMusicRemote_RegisterSteamMusicRemote { { 0 } },
    DeregisterSteamMusicRemote = SteamAPI_ISteamMusicRemote_DeregisterSteamMusicRemote { { 0 } },
    BIsCurrentMusicRemote = SteamAPI_ISteamMusicRemote_BIsCurrentMusicRemote { { 0 } },
    BActivationSuccess = SteamAPI_ISteamMusicRemote_BActivationSuccess { { 0 } },
    SetDisplayName = SteamAPI_ISteamMusicRemote_SetDisplayName { { 0 } },
    SetPNGIcon_64x64 = SteamAPI_ISteamMusicRemote_SetPNGIcon_64x64 { { 0 } },
    EnablePlayPrevious = SteamAPI_ISteamMusicRemote_EnablePlayPrevious { { 0 } },
    EnablePlayNext = SteamAPI_ISteamMusicRemote_EnablePlayNext { { 0 } },
    EnableShuffled = SteamAPI_ISteamMusicRemote_EnableShuffled { { 0 } },
    EnableLooped = SteamAPI_ISteamMusicRemote_EnableLooped { { 0 } },
    EnableQueue = SteamAPI_ISteamMusicRemote_EnableQueue { { 0 } },
    EnablePlaylists = SteamAPI_ISteamMusicRemote_EnablePlaylists { { 0 } },
    UpdatePlaybackStatus = SteamAPI_ISteamMusicRemote_UpdatePlaybackStatus { { 0 } },
    UpdateShuffled = SteamAPI_ISteamMusicRemote_UpdateShuffled { { 0 } },
    UpdateLooped = SteamAPI_ISteamMusicRemote_UpdateLooped { { 0 } },
    UpdateVolume = SteamAPI_ISteamMusicRemote_UpdateVolume { { 0 } },
    CurrentEntryWillChange = SteamAPI_ISteamMusicRemote_CurrentEntryWillChange { { 0 } },
    CurrentEntryIsAvailable = SteamAPI_ISteamMusicRemote_CurrentEntryIsAvailable { { 0 } },
    UpdateCurrentEntryText = SteamAPI_ISteamMusicRemote_UpdateCurrentEntryText { { 0 } },
    UpdateCurrentEntryElapsedSeconds = SteamAPI_ISteamMusicRemote_UpdateCurrentEntryElapsedSeconds { { 0 } },
    UpdateCurrentEntryCoverArt = SteamAPI_ISteamMusicRemote_UpdateCurrentEntryCoverArt { { 0 } },
    CurrentEntryDidChange = SteamAPI_ISteamMusicRemote_CurrentEntryDidChange { { 0 } },
    QueueWillChange = SteamAPI_ISteamMusicRemote_QueueWillChange { { 0 } },
    ResetQueueEntries = SteamAPI_ISteamMusicRemote_ResetQueueEntries { { 0 } },
    SetQueueEntry = SteamAPI_ISteamMusicRemote_SetQueueEntry { { 0 } },
    SetCurrentQueueEntry = SteamAPI_ISteamMusicRemote_SetCurrentQueueEntry { { 0 } },
    QueueDidChange = SteamAPI_ISteamMusicRemote_QueueDidChange { { 0 } },
    PlaylistWillChange = SteamAPI_ISteamMusicRemote_PlaylistWillChange { { 0 } },
    ResetPlaylistEntries = SteamAPI_ISteamMusicRemote_ResetPlaylistEntries { { 0 } },
    SetPlaylistEntry = SteamAPI_ISteamMusicRemote_SetPlaylistEntry { { 0 } },
    SetCurrentPlaylistEntry = SteamAPI_ISteamMusicRemote_SetCurrentPlaylistEntry { { 0 } },
    PlaylistDidChange = SteamAPI_ISteamMusicRemote_PlaylistDidChange { { 0 } },
}

// ---------------------------------------------------------------- ISteamHTMLSurface

def_iface! { VHTMLSurface, VHTMLSURFACE, HTMLSURFACE_OBJ, ps, pa, pb, pc, pd, pe,
    Init = SteamAPI_ISteamHTMLSurface_Init { { 1 } },
    Shutdown = SteamAPI_ISteamHTMLSurface_Shutdown { { 1 } },
    CreateBrowser = SteamAPI_ISteamHTMLSurface_CreateBrowser { { next_call() } },
    RemoveBrowser = SteamAPI_ISteamHTMLSurface_RemoveBrowser { { 0 } },
    LoadURL = SteamAPI_ISteamHTMLSurface_LoadURL { { 0 } },
    SetSize = SteamAPI_ISteamHTMLSurface_SetSize { { 0 } },
    StopLoad = SteamAPI_ISteamHTMLSurface_StopLoad { { 0 } },
    Reload = SteamAPI_ISteamHTMLSurface_Reload { { 0 } },
    GoBack = SteamAPI_ISteamHTMLSurface_GoBack { { 0 } },
    GoForward = SteamAPI_ISteamHTMLSurface_GoForward { { 0 } },
    AddHeader = SteamAPI_ISteamHTMLSurface_AddHeader { { 0 } },
    ExecuteJavascript = SteamAPI_ISteamHTMLSurface_ExecuteJavascript { { 0 } },
    MouseUp = SteamAPI_ISteamHTMLSurface_MouseUp { { 0 } },
    MouseDown = SteamAPI_ISteamHTMLSurface_MouseDown { { 0 } },
    MouseDoubleClick = SteamAPI_ISteamHTMLSurface_MouseDoubleClick { { 0 } },
    MouseMove = SteamAPI_ISteamHTMLSurface_MouseMove { { 0 } },
    MouseWheel = SteamAPI_ISteamHTMLSurface_MouseWheel { { 0 } },
    KeyDown = SteamAPI_ISteamHTMLSurface_KeyDown { { 0 } },
    KeyUp = SteamAPI_ISteamHTMLSurface_KeyUp { { 0 } },
    KeyChar = SteamAPI_ISteamHTMLSurface_KeyChar { { 0 } },
    SetHorizontalScroll = SteamAPI_ISteamHTMLSurface_SetHorizontalScroll { { 0 } },
    SetVerticalScroll = SteamAPI_ISteamHTMLSurface_SetVerticalScroll { { 0 } },
    SetKeyFocus = SteamAPI_ISteamHTMLSurface_SetKeyFocus { { 0 } },
    ViewSource = SteamAPI_ISteamHTMLSurface_ViewSource { { 0 } },
    CopyToClipboard = SteamAPI_ISteamHTMLSurface_CopyToClipboard { { 0 } },
    PasteFromClipboard = SteamAPI_ISteamHTMLSurface_PasteFromClipboard { { 0 } },
    Find = SteamAPI_ISteamHTMLSurface_Find { { 0 } },
    StopFind = SteamAPI_ISteamHTMLSurface_StopFind { { 0 } },
    GetLinkAtPosition = SteamAPI_ISteamHTMLSurface_GetLinkAtPosition { { 0 } },
    SetCookie = SteamAPI_ISteamHTMLSurface_SetCookie { { 0 } },
    SetPageScaleFactor = SteamAPI_ISteamHTMLSurface_SetPageScaleFactor { { 0 } },
    SetBackgroundMode = SteamAPI_ISteamHTMLSurface_SetBackgroundMode { { 0 } },
    AllowStartRequest = SteamAPI_ISteamHTMLSurface_AllowStartRequest { { 0 } },
    JSDialogResponse = SteamAPI_ISteamHTMLSurface_JSDialogResponse { { 0 } },
    FileLoadDialogResponse = SteamAPI_ISteamHTMLSurface_FileLoadDialogResponse { { 0 } },
    SetDPIScalingFactor = SteamAPI_ISteamHTMLSurface_SetDPIScalingFactor { { 0 } },
    OpenDeveloperTools = SteamAPI_ISteamHTMLSurface_OpenDeveloperTools { { 0 } },
}

// ---------------------------------------------------------------- ISteamInventory

def_iface! { VInventory, VINVENTORY, INVENTORY_OBJ, ps, pa, pb, pc, pd, pe,
    GetResultStatus = SteamAPI_ISteamInventory_GetResultStatus { { 1 } },
    GetResultItems = SteamAPI_ISteamInventory_GetResultItems { { 0 } },
    GetResultItemProperty = SteamAPI_ISteamInventory_GetResultItemProperty { { static_name(b"\0") } },
    GetResultTimestamp = SteamAPI_ISteamInventory_GetResultTimestamp { { 1664841600 } },
    CheckResultSteamID = SteamAPI_ISteamInventory_CheckResultSteamID { { 1 } },
    DestroyResult = SteamAPI_ISteamInventory_DestroyResult { { 0 } },
    GetAllItems = SteamAPI_ISteamInventory_GetAllItems { { next_call() } },
    GetItemsByID = SteamAPI_ISteamInventory_GetItemsByID { { next_call() } },
    DeserializeResult = SteamAPI_ISteamInventory_DeserializeResult { { 1 } },
    SerializeResult = SteamAPI_ISteamInventory_SerializeResult {
        {
            if pc != 0 { *(pc as *mut u32) = 0; }
            0
        }
    },
    GenerateItems = SteamAPI_ISteamInventory_GenerateItems { { next_call() } },
    GrantPromoItems = SteamAPI_ISteamInventory_GrantPromoItems { { next_call() } },
    AddPromoItem = SteamAPI_ISteamInventory_AddPromoItem { { next_call() } },
    AddPromoItems = SteamAPI_ISteamInventory_AddPromoItems { { next_call() } },
    ConsumeItem = SteamAPI_ISteamInventory_ConsumeItem { { next_call() } },
    ExchangeItems = SteamAPI_ISteamInventory_ExchangeItems { { next_call() } },
    TransferItemQuantity = SteamAPI_ISteamInventory_TransferItemQuantity { { next_call() } },
    SendItemDropHeartbeat = SteamAPI_ISteamInventory_SendItemDropHeartbeat { { 0 } },
    TriggerItemDrop = SteamAPI_ISteamInventory_TriggerItemDrop { { next_call() } },
    TradeItems = SteamAPI_ISteamInventory_TradeItems { { next_call() } },
    LoadItemDefinitions = SteamAPI_ISteamInventory_LoadItemDefinitions { { 1 } },
    GetItemDefinitionIDs = SteamAPI_ISteamInventory_GetItemDefinitionIDs { { 0 } },
    GetItemDefinitionProperty = SteamAPI_ISteamInventory_GetItemDefinitionProperty { { 0 } },
    RequestEligiblePromoItemDefinitionsIDs = SteamAPI_ISteamInventory_RequestEligiblePromoItemDefinitionsIDs { { next_call() } },
    GetEligiblePromoItemDefinitionIDs = SteamAPI_ISteamInventory_GetEligiblePromoItemDefinitionIDs { { 0 } },
    StartPurchase = SteamAPI_ISteamInventory_StartPurchase { { next_call() } },
    RequestPrices = SteamAPI_ISteamInventory_RequestPrices { { next_call() } },
    GetNumItemsWithPrices = SteamAPI_ISteamInventory_GetNumItemsWithPrices { { 0 } },
    GetItemsWithPrices = SteamAPI_ISteamInventory_GetItemsWithPrices { { 0 } },
    GetItemPrice = SteamAPI_ISteamInventory_GetItemPrice { { 0 } },
    StartUpdateProperties = SteamAPI_ISteamInventory_StartUpdateProperties { { next_call() } },
    RemoveProperty = SteamAPI_ISteamInventory_RemoveProperty { { 1 } },
    SetPropertyInt64 = SteamAPI_ISteamInventory_SetPropertyInt64 { { 1 } },
    SetPropertyFloat = SteamAPI_ISteamInventory_SetPropertyFloat { { 1 } },
    SetPropertyBool = SteamAPI_ISteamInventory_SetPropertyBool { { 1 } },
    SetPropertyString = SteamAPI_ISteamInventory_SetPropertyString { { 1 } },
    SubmitUpdateProperties = SteamAPI_ISteamInventory_SubmitUpdateProperties { { next_call() } },
}

// ---------------------------------------------------------------- ISteamVideo

def_iface! { VVideo, VVIDEO, VIDEO_OBJ, ps, pa, pb, pc, pd, pe,
    GetVideoURL = SteamAPI_ISteamVideo_GetVideoURL { { 0 } },
    IsBroadcasting = SteamAPI_ISteamVideo_IsBroadcasting { { 0 } },
    GetOPFSettings = SteamAPI_ISteamVideo_GetOPFSettings { { 0 } },
    GetOPFStringForApp = SteamAPI_ISteamVideo_GetOPFStringForApp { { -1i64 as u64 } },
}

// ---------------------------------------------------------------- ISteamParentalSettings

def_iface! { VParental, VPARENTAL, PARENTAL_OBJ, ps, pa, pb, pc, pd, pe,
    BIsParentalLockEnabled = SteamAPI_ISteamParentalSettings_BIsParentalLockEnabled { { 0 } },
    BIsParentalLockLocked = SteamAPI_ISteamParentalSettings_BIsParentalLockLocked { { 0 } },
    BIsAppBlocked = SteamAPI_ISteamParentalSettings_BIsAppBlocked { { 0 } },
    BIsAppInBlockList = SteamAPI_ISteamParentalSettings_BIsAppInBlockList { { 0 } },
    BIsFeatureBlocked = SteamAPI_ISteamParentalSettings_BIsFeatureBlocked { { 0 } },
    BIsFeatureInBlockList = SteamAPI_ISteamParentalSettings_BIsFeatureInBlockList { { 0 } },
}

// ---------------------------------------------------------------- ISteamGameServerStats

def_iface! { VGameServerStats, VGAMESERVERSTATS, GAMESERVERSTATS_OBJ, ps, pa, pb, pc, pd, pe,
    RequestUserStats = SteamAPI_ISteamGameServerStats_RequestUserStats { { 1 } },
    GetUserStatInt32 = SteamAPI_ISteamGameServerStats_GetUserStatInt32 {
        {
            if pc != 0 { *(pc as *mut i32) = 0; }
            1
        }
    },
    GetUserStatFloat = SteamAPI_ISteamGameServerStats_GetUserStatFloat {
        {
            if pc != 0 { *(pc as *mut f32) = 0.0; }
            1
        }
    },
    GetUserAchievement = SteamAPI_ISteamGameServerStats_GetUserAchievement { { 0 } },
    SetUserStatInt32 = SteamAPI_ISteamGameServerStats_SetUserStatInt32 { { 1 } },
    SetUserStatFloat = SteamAPI_ISteamGameServerStats_SetUserStatFloat { { 1 } },
    UpdateUserAvgRateStat = SteamAPI_ISteamGameServerStats_UpdateUserAvgRateStat { { 1 } },
    SetUserAchievement = SteamAPI_ISteamGameServerStats_SetUserAchievement { { 1 } },
    ClearUserAchievement = SteamAPI_ISteamGameServerStats_ClearUserAchievement { { 1 } },
    StoreUserStats = SteamAPI_ISteamGameServerStats_StoreUserStats { { 1 } },
}

// ---------------------------------------------------------------- ISteamGameServer

def_iface! { VGameServer, VGAMESERVER, GAMESERVER_OBJ, ps, pa, pb, pc, pd, pe,
    SetProduct = SteamAPI_ISteamGameServer_SetProduct { { 0 } },
    SetGameDescription = SteamAPI_ISteamGameServer_SetGameDescription { { 0 } },
    SetModDir = SteamAPI_ISteamGameServer_SetModDir { { 0 } },
    SetDedicatedServer = SteamAPI_ISteamGameServer_SetDedicatedServer { { 0 } },
    LogOn = SteamAPI_ISteamGameServer_LogOn { { 0 } },
    LogOnAnonymous = SteamAPI_ISteamGameServer_LogOnAnonymous { { 0 } },
    LogOff = SteamAPI_ISteamGameServer_LogOff { { 0 } },
    BLoggedOn = SteamAPI_ISteamGameServer_BLoggedOn { { 1 } },
    BSecure = SteamAPI_ISteamGameServer_BSecure { { 1 } },
    GetSteamID = SteamAPI_ISteamGameServer_GetSteamID { { FAKE_STEAM_ID } },
    WasRestartRequested = SteamAPI_ISteamGameServer_WasRestartRequested { { 0 } },
    SetMaxPlayerCount = SteamAPI_ISteamGameServer_SetMaxPlayerCount { { 0 } },
    SetBotPlayerCount = SteamAPI_ISteamGameServer_SetBotPlayerCount { { 0 } },
    SetServerName = SteamAPI_ISteamGameServer_SetServerName { { 0 } },
    SetMapName = SteamAPI_ISteamGameServer_SetMapName { { 0 } },
    SetPasswordProtected = SteamAPI_ISteamGameServer_SetPasswordProtected { { 0 } },
    SetSpectatorPort = SteamAPI_ISteamGameServer_SetSpectatorPort { { 0 } },
    SetSpectatorServerName = SteamAPI_ISteamGameServer_SetSpectatorServerName { { 0 } },
    ClearAllKeyValues = SteamAPI_ISteamGameServer_ClearAllKeyValues { { 0 } },
    SetKeyValue = SteamAPI_ISteamGameServer_SetKeyValue { { 0 } },
    SetGameTags = SteamAPI_ISteamGameServer_SetGameTags { { 0 } },
    SetGameData = SteamAPI_ISteamGameServer_SetGameData { { 0 } },
    SetRegion = SteamAPI_ISteamGameServer_SetRegion { { 0 } },
    GetAuthSessionTicket = SteamAPI_ISteamGameServer_GetAuthSessionTicket {
        {
            let n = TICKET_COUNTER.fetch_add(1, Ordering::Relaxed);
            let size: u32 = 100;
            if pa != 0 && (pb as u32) >= size {
                let p = pa as *mut u8;
                std::ptr::write_bytes(p, 0, size as usize);
                std::ptr::copy_nonoverlapping(b"FLUXREC-GS-TICKET".as_ptr(), p, 17);
                std::ptr::write(p.add(20), (n & 0xFF) as u8);
                std::ptr::write(p.add(21), ((n >> 8) & 0xFF) as u8);
                std::ptr::write(p.add(22), ((n >> 16) & 0xFF) as u8);
            }
            if pc != 0 { *(pc as *mut u32) = size; }
            0x46554C58_0000_0000u64 | (n as u64)
        }
    },
    BeginAuthSession = SteamAPI_ISteamGameServer_BeginAuthSession { { 0 } },
    EndAuthSession = SteamAPI_ISteamGameServer_EndAuthSession { { 0 } },
    CancelAuthTicket = SteamAPI_ISteamGameServer_CancelAuthTicket { { 0 } },
    UserHasLicenseForApp = SteamAPI_ISteamGameServer_UserHasLicenseForApp { { 1 } },
    RequestUserGroupStatus = SteamAPI_ISteamGameServer_RequestUserGroupStatus { { 1 } },
    GetGameplayStats = SteamAPI_ISteamGameServer_GetGameplayStats { { 0 } },
    GetServerReputation = SteamAPI_ISteamGameServer_GetServerReputation { { next_call() } },
    GetPublicIP = SteamAPI_ISteamGameServer_GetPublicIP { { 0x7F000001 } },
    HandleIncomingPacket = SteamAPI_ISteamGameServer_HandleIncomingPacket { { 0 } },
    GetNextOutgoingPacket = SteamAPI_ISteamGameServer_GetNextOutgoingPacket { { 0 } },
    EnableHeartbeats = SteamAPI_ISteamGameServer_EnableHeartbeats { { 0 } },
    SetHeartbeatInterval = SteamAPI_ISteamGameServer_SetHeartbeatInterval { { 0 } },
    ForceHeartbeat = SteamAPI_ISteamGameServer_ForceHeartbeat { { 0 } },
    AssociateWithClan = SteamAPI_ISteamGameServer_AssociateWithClan { { next_call() } },
    ComputeNewPlayerCompatibility = SteamAPI_ISteamGameServer_ComputeNewPlayerCompatibility { { next_call() } },
    SendUserConnectAndAuthenticate = SteamAPI_ISteamGameServer_SendUserConnectAndAuthenticate { { 1 } },
    CreateUnauthenticatedUserConnection = SteamAPI_ISteamGameServer_CreateUnauthenticatedUserConnection { { FAKE_STEAM_ID } },
    SendUserDisconnect = SteamAPI_ISteamGameServer_SendUserDisconnect { { 0 } },
    BUpdateUserData = SteamAPI_ISteamGameServer_BUpdateUserData { { 1 } },
}

// ---------------------------------------------------------------- matchmaking server-list response handlers

def_iface! { VMMPingResp, VMMPINGRESP, MMPINGRESP_OBJ, ps, pa, pb, pc, pd, pe,
    ServerResponded = SteamAPI_ISteamMatchmakingPingResponse_ServerResponded { { 0 } },
    ServerFailedToRespond = SteamAPI_ISteamMatchmakingPingResponse_ServerFailedToRespond { { 0 } },
}

def_iface! { VMMPlayersResp, VMMPLAYERSRESP, MMPLAYERSRESP_OBJ, ps, pa, pb, pc, pd, pe,
    AddPlayerToList = SteamAPI_ISteamMatchmakingPlayersResponse_AddPlayerToList { { 0 } },
    PlayersFailedToRespond = SteamAPI_ISteamMatchmakingPlayersResponse_PlayersFailedToRespond { { 0 } },
    PlayersRefreshComplete = SteamAPI_ISteamMatchmakingPlayersResponse_PlayersRefreshComplete { { 0 } },
}

def_iface! { VMMRulesResp, VMMRULESRESP, MMRULESRESP_OBJ, ps, pa, pb, pc, pd, pe,
    RulesResponded = SteamAPI_ISteamMatchmakingRulesResponse_RulesResponded { { 0 } },
    RulesFailedToRespond = SteamAPI_ISteamMatchmakingRulesResponse_RulesFailedToRespond { { 0 } },
    RulesRefreshComplete = SteamAPI_ISteamMatchmakingRulesResponse_RulesRefreshComplete { { 0 } },
}

def_iface! { VMMServerListResp, VMMSERVERLISTRESP, MMSERVERLISTRESP_OBJ, ps, pa, pb, pc, pd, pe,
    ServerResponded = SteamAPI_ISteamMatchmakingServerListResponse_ServerResponded { { 0 } },
    ServerFailedToRespond = SteamAPI_ISteamMatchmakingServerListResponse_ServerFailedToRespond { { 0 } },
    RefreshComplete = SteamAPI_ISteamMatchmakingServerListResponse_RefreshComplete { { 0 } },
}

// ---------------------------------------------------------------- ISteamNetworkingSockets

def_iface! { VNetSockets, VNETSOCKETS, NETSOCKETS_OBJ, ps, pa, pb, pc, pd, pe,
    CreateListenSocketIP = SteamAPI_ISteamNetworkingSockets_CreateListenSocketIP { { 1 } },
    ConnectByIPAddress = SteamAPI_ISteamNetworkingSockets_ConnectByIPAddress { { 2 } },
    CreateListenSocketP2P = SteamAPI_ISteamNetworkingSockets_CreateListenSocketP2P { { 4 } },
    ConnectP2P = SteamAPI_ISteamNetworkingSockets_ConnectP2P { { 5 } },
    AcceptConnection = SteamAPI_ISteamNetworkingSockets_AcceptConnection { { 1 } },
    CloseConnection = SteamAPI_ISteamNetworkingSockets_CloseConnection { { 1 } },
    CloseListenSocket = SteamAPI_ISteamNetworkingSockets_CloseListenSocket { { 1 } },
    SetConnectionUserData = SteamAPI_ISteamNetworkingSockets_SetConnectionUserData { { 1 } },
    GetConnectionUserData = SteamAPI_ISteamNetworkingSockets_GetConnectionUserData { { 0 } },
    SetConnectionName = SteamAPI_ISteamNetworkingSockets_SetConnectionName { { 0 } },
    GetConnectionName = SteamAPI_ISteamNetworkingSockets_GetConnectionName { { 0 } },
    SendMessageToConnection = SteamAPI_ISteamNetworkingSockets_SendMessageToConnection { { 0 } },
    SendMessages = SteamAPI_ISteamNetworkingSockets_SendMessages { { 0 } },
    FlushMessagesOnConnection = SteamAPI_ISteamNetworkingSockets_FlushMessagesOnConnection { { 1 } },
    ReceiveMessagesOnConnection = SteamAPI_ISteamNetworkingSockets_ReceiveMessagesOnConnection { { 0 } },
    GetConnectionInfo = SteamAPI_ISteamNetworkingSockets_GetConnectionInfo { { 0 } },
    GetQuickConnectionStatus = SteamAPI_ISteamNetworkingSockets_GetQuickConnectionStatus { { 0 } },
    GetDetailedConnectionStatus = SteamAPI_ISteamNetworkingSockets_GetDetailedConnectionStatus { { 0 } },
    GetListenSocketAddress = SteamAPI_ISteamNetworkingSockets_GetListenSocketAddress { { 0 } },
    CreateSocketPair = SteamAPI_ISteamNetworkingSockets_CreateSocketPair { { 1 } },
    GetIdentity = SteamAPI_ISteamNetworkingSockets_GetIdentity { { 1 } },
    InitAuthentication = SteamAPI_ISteamNetworkingSockets_InitAuthentication { { 1 } },
    GetAuthenticationStatus = SteamAPI_ISteamNetworkingSockets_GetAuthenticationStatus { { 1 } },
    SetCertificate = SteamAPI_ISteamNetworkingSockets_SetCertificate { { 1 } },
    GetCertificateRequest = SteamAPI_ISteamNetworkingSockets_GetCertificateRequest { { 0 } },
    CreatePollGroup = SteamAPI_ISteamNetworkingSockets_CreatePollGroup { { 6 } },
    DestroyPollGroup = SteamAPI_ISteamNetworkingSockets_DestroyPollGroup { { 1 } },
    ReceiveMessagesOnPollGroup = SteamAPI_ISteamNetworkingSockets_ReceiveMessagesOnPollGroup { { 0 } },
    ReceivedRelayAuthTicket = SteamAPI_ISteamNetworkingSockets_ReceivedRelayAuthTicket { { 1 } },
    FindRelayAuthTicketForServer = SteamAPI_ISteamNetworkingSockets_FindRelayAuthTicketForServer { { 0 } },
    ConnectToHostedDedicatedServer = SteamAPI_ISteamNetworkingSockets_ConnectToHostedDedicatedServer { { 7 } },
    GetHostedDedicatedServerPort = SteamAPI_ISteamNetworkingSockets_GetHostedDedicatedServerPort { { 0 } },
    GetHostedDedicatedServerPOPID = SteamAPI_ISteamNetworkingSockets_GetHostedDedicatedServerPOPID { { 0 } },
    GetHostedDedicatedServerAddress = SteamAPI_ISteamNetworkingSockets_GetHostedDedicatedServerAddress { { 0 } },
    CreateHostedDedicatedServerListenSocket = SteamAPI_ISteamNetworkingSockets_CreateHostedDedicatedServerListenSocket { { 8 } },
    GetGameCoordinatorServerLogin = SteamAPI_ISteamNetworkingSockets_GetGameCoordinatorServerLogin { { 0 } },
    ConnectP2PCustomSignaling = SteamAPI_ISteamNetworkingSockets_ConnectP2PCustomSignaling { { 9 } },
    ReceivedP2PCustomSignal = SteamAPI_ISteamNetworkingSockets_ReceivedP2PCustomSignal { { 1 } },
    SetConnectionPollGroup = SteamAPI_ISteamNetworkingSockets_SetConnectionPollGroup { { 1 } },
}

// ---------------------------------------------------------------- ISteamNetworkingUtils

def_iface! { VNetUtils, VNETUTILS, NETUTILS_OBJ, ps, pa, pb, pc, pd, pe,
    AllocateMessage = SteamAPI_ISteamNetworkingUtils_AllocateMessage { { 0 } },
    InitRelayNetworkAccess = SteamAPI_ISteamNetworkingUtils_InitRelayNetworkAccess { { 0 } },
    GetRelayNetworkStatus = SteamAPI_ISteamNetworkingUtils_GetRelayNetworkStatus { { 1 } },
    GetLocalPingLocation = SteamAPI_ISteamNetworkingUtils_GetLocalPingLocation { { 1 } },
    EstimatePingTimeBetweenTwoLocations = SteamAPI_ISteamNetworkingUtils_EstimatePingTimeBetweenTwoLocations { { 30 } },
    EstimatePingTimeFromLocalHost = SteamAPI_ISteamNetworkingUtils_EstimatePingTimeFromLocalHost { { 30 } },
    ConvertPingLocationToString = SteamAPI_ISteamNetworkingUtils_ConvertPingLocationToString { { 0 } },
    ParsePingLocationString = SteamAPI_ISteamNetworkingUtils_ParsePingLocationString { { 0 } },
    CheckPingDataUpToDate = SteamAPI_ISteamNetworkingUtils_CheckPingDataUpToDate { { 1 } },
    GetPingToDataCenter = SteamAPI_ISteamNetworkingUtils_GetPingToDataCenter { { 30 } },
    GetDirectPingToPOP = SteamAPI_ISteamNetworkingUtils_GetDirectPingToPOP { { 30 } },
    GetPOPCount = SteamAPI_ISteamNetworkingUtils_GetPOPCount { { 0 } },
    GetPOPList = SteamAPI_ISteamNetworkingUtils_GetPOPList { { 0 } },
    GetLocalTimestamp = SteamAPI_ISteamNetworkingUtils_GetLocalTimestamp { { 1664841600000000 } },
    SetDebugOutputFunction = SteamAPI_ISteamNetworkingUtils_SetDebugOutputFunction { { 0 } },
    SetGlobalConfigValueInt32 = SteamAPI_ISteamNetworkingUtils_SetGlobalConfigValueInt32 { { 1 } },
    SetGlobalConfigValueFloat = SteamAPI_ISteamNetworkingUtils_SetGlobalConfigValueFloat { { 1 } },
    SetGlobalConfigValueString = SteamAPI_ISteamNetworkingUtils_SetGlobalConfigValueString { { 1 } },
    SetConnectionConfigValueInt32 = SteamAPI_ISteamNetworkingUtils_SetConnectionConfigValueInt32 { { 1 } },
    SetConnectionConfigValueFloat = SteamAPI_ISteamNetworkingUtils_SetConnectionConfigValueFloat { { 1 } },
    SetConnectionConfigValueString = SteamAPI_ISteamNetworkingUtils_SetConnectionConfigValueString { { 1 } },
    SetConfigValue = SteamAPI_ISteamNetworkingUtils_SetConfigValue { { 1 } },
    SetConfigValueStruct = SteamAPI_ISteamNetworkingUtils_SetConfigValueStruct { { 1 } },
    GetConfigValue = SteamAPI_ISteamNetworkingUtils_GetConfigValue { { 0 } },
    GetConfigValueInfo = SteamAPI_ISteamNetworkingUtils_GetConfigValueInfo { { 0 } },
    SteamNetworkingIPAddr_ParseString = SteamAPI_ISteamNetworkingUtils_SteamNetworkingIPAddr_ParseString { { 1 } },
    SteamNetworkingIPAddr_ToString = SteamAPI_ISteamNetworkingUtils_SteamNetworkingIPAddr_ToString { { 0 } },
    SteamNetworkingIdentity_ParseString = SteamAPI_ISteamNetworkingUtils_SteamNetworkingIdentity_ParseString { { 1 } },
    SteamNetworkingIdentity_ToString = SteamAPI_ISteamNetworkingUtils_SteamNetworkingIdentity_ToString { { 0 } },
    GetFirstConfigValue = SteamAPI_ISteamNetworkingUtils_GetFirstConfigValue { { 0 } },
}

// ---------------------------------------------------------------- custom signaling helpers

def_iface! { VNetConnSig, VNETCONNSIG, NETCONNSIG_OBJ, ps, pa, pb, pc, pd, pe,
    SendSignal = SteamAPI_ISteamNetworkingConnectionCustomSignaling_SendSignal { { 1 } },
    Release = SteamAPI_ISteamNetworkingConnectionCustomSignaling_Release { { 1 } },
}

def_iface! { VNetSigRecv, VNETSIGRECV, NETSIGRECV_OBJ, ps, pa, pb, pc, pd, pe,
    OnConnectRequest = SteamAPI_ISteamNetworkingCustomSignalingRecvContext_OnConnectRequest { { 1 } },
    SendRejectionSignal = SteamAPI_ISteamNetworkingCustomSignalingRecvContext_SendRejectionSignal { { 1 } },
}

// ---------------------------------------------------------------- ISteamParties

def_iface! { VParties, VPARTIES, PARTIES_OBJ, ps, pa, pb, pc, pd, pe,
    GetNumActiveBeacons = SteamAPI_ISteamParties_GetNumActiveBeacons { { 0 } },
    GetBeaconByIndex = SteamAPI_ISteamParties_GetBeaconByIndex { { 0 } },
    GetBeaconDetails = SteamAPI_ISteamParties_GetBeaconDetails { { 0 } },
    JoinParty = SteamAPI_ISteamParties_JoinParty { { next_call() } },
    GetNumAvailableBeaconLocations = SteamAPI_ISteamParties_GetNumAvailableBeaconLocations { { 0 } },
    GetAvailableBeaconLocations = SteamAPI_ISteamParties_GetAvailableBeaconLocations { { 0 } },
    CreateBeacon = SteamAPI_ISteamParties_CreateBeacon { { next_call() } },
    OnReservationCompleted = SteamAPI_ISteamParties_OnReservationCompleted { { 0 } },
    CancelReservation = SteamAPI_ISteamParties_CancelReservation { { 0 } },
    ChangeNumOpenSlots = SteamAPI_ISteamParties_ChangeNumOpenSlots { { next_call() } },
    DestroyBeacon = SteamAPI_ISteamParties_DestroyBeacon { { 0 } },
    GetBeaconLocationData = SteamAPI_ISteamParties_GetBeaconLocationData { { 0 } },
}

// ---------------------------------------------------------------- ISteamRemotePlay

def_iface! { VRemotePlay, VREMOTEPLAY, REMOTEPLAY_OBJ, ps, pa, pb, pc, pd, pe,
    GetSessionCount = SteamAPI_ISteamRemotePlay_GetSessionCount { { 0 } },
    GetSessionID = SteamAPI_ISteamRemotePlay_GetSessionID { { 0 } },
    GetSessionSteamID = SteamAPI_ISteamRemotePlay_GetSessionSteamID { { 0 } },
    GetSessionClientName = SteamAPI_ISteamRemotePlay_GetSessionClientName { { static_name(b"\0") } },
    GetSessionClientFormFactor = SteamAPI_ISteamRemotePlay_GetSessionClientFormFactor { { 0 } },
    BGetSessionClientResolution = SteamAPI_ISteamRemotePlay_BGetSessionClientResolution { { 0 } },
    BSendRemotePlayTogetherInvite = SteamAPI_ISteamRemotePlay_BSendRemotePlayTogetherInvite { { 0 } },
}

// ---------------------------------------------------------------- ISteamTV

def_iface! { VTV, VTV, TV_OBJ, ps, pa, pb, pc, pd, pe,
    IsBroadcasting = SteamAPI_ISteamTV_IsBroadcasting { { 0 } },
    AddBroadcastGameData = SteamAPI_ISteamTV_AddBroadcastGameData { { 0 } },
    RemoveBroadcastGameData = SteamAPI_ISteamTV_RemoveBroadcastGameData { { 0 } },
    AddRegion = SteamAPI_ISteamTV_AddRegion { { 0 } },
    RemoveRegion = SteamAPI_ISteamTV_RemoveRegion { { 0 } },
    AddTimelineMarker = SteamAPI_ISteamTV_AddTimelineMarker { { 0 } },
    RemoveTimelineMarker = SteamAPI_ISteamTV_RemoveTimelineMarker { { 0 } },
}

// ---------------------------------------------------------------- interface dispatch

fn find_interface(ver: &str) -> *const c_void {
    let o: *const IfaceObj = match ver.as_bytes() {
        b"SteamClient020" => &CLIENT_OBJ,
        b"SteamUser020" => &USER_OBJ,
        b"SteamFriends017" => &FRIENDS_OBJ,
        b"SteamUtils009" => &UTILS_OBJ,
        b"SteamMatchmaking009" => &MATCHMAKING_OBJ,
        b"SteamMatchmakingServers002" => &MMSERVERS_OBJ,
        b"SteamGameSearch001" => &GAMESEARCH_OBJ,
        b"SteamUserStats011" => &USERSTATS_OBJ,
        b"SteamApps008" => &APPS_OBJ,
        b"SteamNetworking006" => &NETWORKING_OBJ,
        b"SteamRemoteStorage014" => &REMOTESTORAGE_OBJ,
        b"SteamScreenshots003" => &SCREENSHOTS_OBJ,
        b"SteamHTTP003" => &HTTP_OBJ,
        b"SteamInput001" => &INPUT_OBJ,
        b"SteamController007" => &CONTROLLER_OBJ,
        b"SteamUGC014" => &UGC_OBJ,
        b"SteamAppList001" => &APPLIST_OBJ,
        b"SteamMusic001" => &MUSIC_OBJ,
        b"SteamMusicRemote001" => &MUSICREMOTE_OBJ,
        b"SteamHTMLSurface005" => &HTMLSURFACE_OBJ,
        b"SteamInventory003" => &INVENTORY_OBJ,
        b"SteamVideo002" => &VIDEO_OBJ,
        b"SteamParentalSettings001" => &PARENTAL_OBJ,
        b"SteamParties002" => &PARTIES_OBJ,
        b"SteamRemotePlay001" => &REMOTEPLAY_OBJ,
        b"SteamTV001" => &TV_OBJ,
        b"SteamGameServer013" => &GAMESERVER_OBJ,
        b"SteamGameServerStats001" => &GAMESERVERSTATS_OBJ,
        b"SteamGameServerUtils009" => &UTILS_OBJ,
        b"SteamGameServerApps008" => &APPS_OBJ,
        b"SteamGameServerNetworking006" => &NETWORKING_OBJ,
        b"SteamGameServerHTTP003" => &HTTP_OBJ,
        b"SteamGameServerInventory003" => &INVENTORY_OBJ,
        b"SteamGameServerUGC014" => &UGC_OBJ,
        b"SteamGameServerNetworkingSockets008" => &NETSOCKETS_OBJ,
        b"SteamNetworkingSockets008" => &NETSOCKETS_OBJ,
        b"SteamNetworkingUtils003" => &NETUTILS_OBJ,
        b"SteamNetworkingMessages002" => &NETSOCKETS_OBJ,
        b"SteamNetworkingConnectionCustomSignaling002" => &NETCONNSIG_OBJ,
        b"SteamNetworkingCustomSignalingRecvContext002" => &NETSIGRECV_OBJ,
        b"SteamMatchmakingPingResponse002" => &MMPINGRESP_OBJ,
        b"SteamMatchmakingPlayersResponse002" => &MMPLAYERSRESP_OBJ,
        b"SteamMatchmakingRulesResponse001" => &MMRULESRESP_OBJ,
        b"SteamMatchmakingServerListResponse001" => &MMSERVERLISTRESP_OBJ,
        _ => {
            // Heuristic fallback: never return null for a plausible name.
            let v = ver;
            let obj = if v.contains("GameServerStats") { &GAMESERVERSTATS_OBJ as *const IfaceObj }
                else if v.contains("GameServer") { &GAMESERVER_OBJ as *const IfaceObj }
                else if v.contains("MatchmakingServers") { &MMSERVERS_OBJ as *const IfaceObj }
                else if v.contains("Matchmaking") { &MATCHMAKING_OBJ as *const IfaceObj }
                else if v.contains("UserStats") { &USERSTATS_OBJ as *const IfaceObj }
                else if v.contains("GameSearch") { &GAMESEARCH_OBJ as *const IfaceObj }
                else if v.contains("RemoteStorage") { &REMOTESTORAGE_OBJ as *const IfaceObj }
                else if v.contains("HTMLSurface") { &HTMLSURFACE_OBJ as *const IfaceObj }
                else if v.contains("MusicRemote") { &MUSICREMOTE_OBJ as *const IfaceObj }
                else if v.contains("Music") { &MUSIC_OBJ as *const IfaceObj }
                else if v.contains("Controller") { &CONTROLLER_OBJ as *const IfaceObj }
                else if v.contains("Input") { &INPUT_OBJ as *const IfaceObj }
                else if v.contains("Friends") { &FRIENDS_OBJ as *const IfaceObj }
                else if v.contains("Screenshots") { &SCREENSHOTS_OBJ as *const IfaceObj }
                else if v.contains("Parental") { &PARENTAL_OBJ as *const IfaceObj }
                else if v.contains("RemotePlay") { &REMOTEPLAY_OBJ as *const IfaceObj }
                else if v.contains("Parties") { &PARTIES_OBJ as *const IfaceObj }
                else if v.contains("Inventory") { &INVENTORY_OBJ as *const IfaceObj }
                else if v.contains("Video") { &VIDEO_OBJ as *const IfaceObj }
                else if v.contains("AppList") { &APPLIST_OBJ as *const IfaceObj }
                else if v.contains("Apps") { &APPS_OBJ as *const IfaceObj }
                else if v.contains("NetworkingSockets") { &NETSOCKETS_OBJ as *const IfaceObj }
                else if v.contains("NetworkingUtils") { &NETUTILS_OBJ as *const IfaceObj }
                else if v.contains("Networking") { &NETWORKING_OBJ as *const IfaceObj }
                else if v.contains("Utils") { &UTILS_OBJ as *const IfaceObj }
                else if v.contains("User") { &USER_OBJ as *const IfaceObj }
                else if v.contains("Client") { &CLIENT_OBJ as *const IfaceObj }
                else { &CLIENT_OBJ as *const IfaceObj };
            return obj as *const c_void;
        }
    };
    o as *const c_void
}

macro_rules! accessor {
    ($name:ident, $obj:ident) => {
        #[no_mangle]
        pub unsafe extern "system" fn $name() -> *mut c_void {
            &$obj as *const _ as *mut c_void
        }
    };
}

accessor!(SteamAPI_SteamUser_v020, USER_OBJ);
accessor!(SteamAPI_SteamFriends_v017, FRIENDS_OBJ);
accessor!(SteamAPI_SteamUtils_v009, UTILS_OBJ);
accessor!(SteamAPI_SteamMatchmaking_v009, MATCHMAKING_OBJ);
accessor!(SteamAPI_SteamMatchmakingServers_v002, MMSERVERS_OBJ);
accessor!(SteamAPI_SteamGameSearch_v001, GAMESEARCH_OBJ);
accessor!(SteamAPI_SteamUserStats_v011, USERSTATS_OBJ);
accessor!(SteamAPI_SteamApps_v008, APPS_OBJ);
accessor!(SteamAPI_SteamNetworking_v006, NETWORKING_OBJ);
accessor!(SteamAPI_SteamRemoteStorage_v014, REMOTESTORAGE_OBJ);
accessor!(SteamAPI_SteamScreenshots_v003, SCREENSHOTS_OBJ);
accessor!(SteamAPI_SteamHTTP_v003, HTTP_OBJ);
accessor!(SteamAPI_SteamInput_v001, INPUT_OBJ);
accessor!(SteamAPI_SteamController_v007, CONTROLLER_OBJ);
accessor!(SteamAPI_SteamUGC_v014, UGC_OBJ);
accessor!(SteamAPI_SteamAppList_v001, APPLIST_OBJ);
accessor!(SteamAPI_SteamMusic_v001, MUSIC_OBJ);
accessor!(SteamAPI_SteamMusicRemote_v001, MUSICREMOTE_OBJ);
accessor!(SteamAPI_SteamHTMLSurface_v005, HTMLSURFACE_OBJ);
accessor!(SteamAPI_SteamInventory_v003, INVENTORY_OBJ);
accessor!(SteamAPI_SteamVideo_v002, VIDEO_OBJ);
accessor!(SteamAPI_SteamParentalSettings_v001, PARENTAL_OBJ);
accessor!(SteamAPI_SteamParties_v002, PARTIES_OBJ);
accessor!(SteamAPI_SteamRemotePlay_v001, REMOTEPLAY_OBJ);
accessor!(SteamAPI_SteamTV_v001, TV_OBJ);
accessor!(SteamAPI_SteamGameServer_v013, GAMESERVER_OBJ);
accessor!(SteamAPI_SteamGameServerStats_v001, GAMESERVERSTATS_OBJ);
accessor!(SteamAPI_SteamGameServerUtils_v009, UTILS_OBJ);
accessor!(SteamAPI_SteamGameServerApps_v008, APPS_OBJ);
accessor!(SteamAPI_SteamGameServerNetworking_v006, NETWORKING_OBJ);
accessor!(SteamAPI_SteamGameServerHTTP_v003, HTTP_OBJ);
accessor!(SteamAPI_SteamGameServerInventory_v003, INVENTORY_OBJ);
accessor!(SteamAPI_SteamGameServerUGC_v014, UGC_OBJ);
accessor!(SteamAPI_SteamGameServerNetworkingSockets_v008, NETSOCKETS_OBJ);
accessor!(SteamAPI_SteamNetworkingSockets_v008, NETSOCKETS_OBJ);
accessor!(SteamAPI_SteamNetworkingUtils_v003, NETUTILS_OBJ);

// Data export: the original DLL exports this global.
#[no_mangle]
pub static g_pSteamClientGameServer: &'static IfaceObj = &CLIENT_OBJ;

// ---------------------------------------------------------------- global API

static INIT_DONE: AtomicU32 = AtomicU32::new(0);

fn do_init() {
    if INIT_DONE.swap(1, Ordering::SeqCst) == 0 {
        // Emulate SteamServersConnected shortly after init.
        queue_callback(1, CB_STEAM_SERVERS_CONNECTED, next_call(), &[]);
    }
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_Init() -> u8 {
    do_init();
    1
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_InitSafe() -> u64 {
    do_init();
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_InitAnonymousUser() -> u8 {
    do_init();
    1
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_Shutdown() -> u64 {
    INIT_DONE.store(0, Ordering::SeqCst);
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_IsSteamRunning() -> u8 {
    1
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_RestartAppIfNecessary(_appid: u32) -> u8 {
    // Never restart: there is no real Steam to relaunch into.
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_GetHSteamPipe() -> i32 {
    1
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_GetHSteamUser() -> i32 {
    1
}

#[no_mangle]
pub unsafe extern "system" fn GetHSteamPipe() -> i32 {
    1
}

#[no_mangle]
pub unsafe extern "system" fn GetHSteamUser() -> i32 {
    1
}

#[no_mangle]
pub unsafe extern "system" fn SteamClient() -> *mut c_void {
    &CLIENT_OBJ as *const _ as *mut c_void
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_RegisterCallback(p_callback: *mut c_void, i_callback: i32) -> u64 {
    if !p_callback.is_null() {
        let mut regs = REG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        regs.push(CbReg { user: 1, callback: i_callback, func: p_callback, call: 0 });
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_UnregisterCallback(p_callback: *mut c_void) -> u64 {
    let mut regs = REG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    regs.retain(|r| r.func != p_callback || r.call != 0);
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_RegisterCallResult(p_callback: *mut c_void, h_call: u64) -> u64 {
    if !p_callback.is_null() {
        let mut regs = REG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        regs.push(CbReg { user: 1, callback: 0, func: p_callback, call: h_call });
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_UnregisterCallResult(p_callback: *mut c_void, h_call: u64) -> u64 {
    let mut regs = REG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    regs.retain(|r| !(r.func == p_callback && r.call == h_call));
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_RunCallbacks() -> u64 {
    dispatch_queued();
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_ReleaseCurrentThreadMemory() -> u64 {
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_UseBreakpadCrashHandler(
    pa: u64, pb: u64, pc: u64, pd: u64, pe: u64,
) -> u64 {
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_WriteMiniDump(pa: u64, pb: u64, pc: u64) -> u64 {
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SetMiniDumpComment(pa: *const c_char) -> u64 {
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SetTryCatchCallbacks(pa: u64, pb: u64) -> u64 {
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SetBreakpadAppID(pa: u32) -> u64 {
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_GetSteamInstallPath() -> u64 {
    static_name(b"FluxRec\0")
}

// ---------------------------------------------------------------- game server globals

#[no_mangle]
pub unsafe extern "system" fn SteamGameServer_InitSafe(
    pa: u64, pb: u64, pc: u64, pd: u64, pe: u64,
) -> u8 {
    do_init();
    1
}

#[no_mangle]
pub unsafe extern "system" fn SteamInternal_GameServer_Init(
    pa: u64, pb: u64, pc: u64, pd: u64, pe: u64,
) -> u8 {
    do_init();
    1
}

#[no_mangle]
pub unsafe extern "system" fn SteamGameServer_Shutdown() -> u64 {
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamGameServer_GetHSteamPipe() -> i32 {
    1
}

#[no_mangle]
pub unsafe extern "system" fn SteamGameServer_GetHSteamUser() -> i32 {
    1
}

#[no_mangle]
pub unsafe extern "system" fn SteamGameServer_GetIPCCallCount() -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamGameServer_GetSteamID() -> u64 {
    FAKE_STEAM_ID
}

#[no_mangle]
pub unsafe extern "system" fn SteamGameServer_BSecure() -> u8 {
    1
}

#[no_mangle]
pub unsafe extern "system" fn SteamGameServer_RunCallbacks() -> u64 {
    dispatch_queued();
    0
}

// ---------------------------------------------------------------- SteamInternal

#[no_mangle]
pub unsafe extern "system" fn SteamInternal_CreateInterface(p_version: *const c_char) -> *mut c_void {
    find_interface(&cstr(p_version)) as *mut c_void
}

#[no_mangle]
pub unsafe extern "system" fn SteamInternal_FindOrCreateUserInterface(
    _h_user: i32, p_version: *const c_char,
) -> *mut c_void {
    find_interface(&cstr(p_version)) as *mut c_void
}

#[no_mangle]
pub unsafe extern "system" fn SteamInternal_FindOrCreateGameServerInterface(
    _h_user: i32, p_version: *const c_char,
) -> *mut c_void {
    find_interface(&cstr(p_version)) as *mut c_void
}

#[no_mangle]
pub unsafe extern "system" fn SteamInternal_ContextInit(_p: *mut c_void) -> i32 {
    do_init();
    1
}

// ---------------------------------------------------------------- manual dispatch

static mut LAST_CB_PTR: *mut u8 = std::ptr::null_mut();
static mut LAST_CB_LEN: usize = 0;

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_ManualDispatch_Init() -> u64 {
    do_init();
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_ManualDispatch_RunFrame(_pipe: i32) -> u64 {
    dispatch_queued();
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_ManualDispatch_GetNextCallback(
    _pipe: i32, p_msg: *mut CallbackMsg,
) -> u8 {
    let item = {
        let mut q = QUEUE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        q.pop()
    };
    if let Some(item) = item {
        if !p_msg.is_null() {
            (*p_msg).m_hSteamUser = item.user;
            (*p_msg).m_iCallback = item.cb;
            (*p_msg).m_pubParam = item.ptr;
            (*p_msg).m_cubParam = item.len as i32;
        }
        LAST_CB_PTR = item.ptr;
        LAST_CB_LEN = item.len;
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_ManualDispatch_FreeLastCallback(_pipe: i32) -> u64 {
    if !LAST_CB_PTR.is_null() {
        drop(Box::from_raw(std::slice::from_raw_parts_mut(LAST_CB_PTR, LAST_CB_LEN)));
        LAST_CB_PTR = std::ptr::null_mut();
        LAST_CB_LEN = 0;
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_ManualDispatch_GetAPICallResult(
    _pipe: i32, h_call: u64, p_out: *mut c_void, cub_out: i32,
    i_expected: i32, pb_failed: *mut u8,
) -> u8 {
    let mut q = QUEUE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(pos) = q.iter().position(|it| it.call == h_call && it.cb == i_expected) {
        let item = q.remove(pos);
        if !p_out.is_null() && cub_out > 0 {
            let n = (item.len as i32).min(cub_out) as usize;
            std::ptr::copy_nonoverlapping(item.ptr, p_out as *mut u8, n);
        }
        if !pb_failed.is_null() {
            *pb_failed = 0;
        }
        drop(Box::from_raw(std::slice::from_raw_parts_mut(item.ptr, item.len)));
        1
    } else {
        0
    }
}

// ---------------------------------------------------------------- struct helpers

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamIPAddress_t_IsSet(p: *const u8) -> u8 {
    if p.is_null() { 0 }
    else { (!std::slice::from_raw_parts(p, 18).iter().all(|&x| x == 0)) as u8 }
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIPAddr_Clear(p: *mut u8) -> u64 {
    if !p.is_null() { std::ptr::write_bytes(p, 0, 20); }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIPAddr_GetIPv4(p: *const u8) -> u32 {
    if p.is_null() { 0 } else { *(p as *const u32) }
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIPAddr_SetIPv4(
    p: *mut u8, ip: u32, port: u16,
) -> u64 {
    if !p.is_null() {
        std::ptr::write_bytes(p, 0, 20);
        *(p as *mut u32) = ip;
        *((p.add(18)) as *mut u16) = port;
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIPAddr_IsEqualTo(
    a: *const u8, b: *const u8,
) -> u8 {
    if a.is_null() || b.is_null() { 0 }
    else { (std::slice::from_raw_parts(a, 20) == std::slice::from_raw_parts(b, 20)) as u8 }
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIPAddr_IsIPv4(p: *const u8) -> u8 {
    let _ = p;
    1
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIPAddr_IsIPv6AllZeros(p: *const u8) -> u8 {
    if p.is_null() { 1 }
    else { (std::slice::from_raw_parts(p, 16).iter().all(|&x| x == 0)) as u8 }
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIPAddr_IsLocalHost(p: *const u8) -> u8 {
    if p.is_null() { 0 } else { ((*(p as *const u32)) == 0x0100007F) as u8 }
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIPAddr_ParseString(
    p: *mut u8, s: *const c_char,
) -> u8 {
    let _ = s;
    SteamAPI_SteamNetworkingIPAddr_Clear(p);
    1
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIPAddr_ToString(
    _p: *const u8, buf: *mut c_char, bufsize: u32, _with_port: u8,
) -> u64 {
    if !buf.is_null() && bufsize > 10 {
        let s = b"127.0.0.1\0";
        let n = s.len().min(bufsize as usize);
        std::ptr::copy_nonoverlapping(s.as_ptr() as *const c_char, buf, n);
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIPAddr_SetIPv6(
    p: *mut u8, ip6: *const u8, port: u16,
) -> u64 {
    if !p.is_null() {
        std::ptr::write_bytes(p, 0, 20);
        if !ip6.is_null() { std::ptr::copy_nonoverlapping(ip6, p, 16); }
        *((p.add(18)) as *mut u16) = port;
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIPAddr_SetIPv6LocalHost(
    p: *mut u8, port: u16,
) -> u64 {
    if !p.is_null() {
        std::ptr::write_bytes(p, 0, 20);
        *p.add(15) = 1;
        *((p.add(18)) as *mut u16) = port;
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIdentity_Clear(p: *mut u8) -> u64 {
    if !p.is_null() { std::ptr::write_bytes(p, 0, 136); }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIdentity_SetSteamID(
    p: *mut u8, sid: u64,
) -> u64 {
    SteamAPI_SteamNetworkingIdentity_SetSteamID64(p, sid)
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIdentity_SetSteamID64(
    p: *mut u8, sid: u64,
) -> u64 {
    if !p.is_null() {
        std::ptr::write_bytes(p, 0, 136);
        *(p as *mut i32) = 1; // k_ESteamNetworkingIdentityType_SteamID
        *((p.add(4)) as *mut i32) = 8;
        *((p.add(8)) as *mut u64) = sid;
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIdentity_GetSteamID64(p: *const u8) -> u64 {
    if p.is_null() || *(p as *const i32) != 1 { 0 } else { *((p.add(8)) as *const u64) }
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIdentity_GetSteamID(p: *const u8) -> u64 {
    SteamAPI_SteamNetworkingIdentity_GetSteamID64(p)
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIdentity_IsInvalid(p: *const u8) -> u8 {
    if p.is_null() { 1 } else { ((*(p as *const i32)) == 0) as u8 }
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIdentity_IsEqualTo(
    a: *const u8, b: *const u8,
) -> u8 {
    if a.is_null() || b.is_null() { 0 }
    else { (std::slice::from_raw_parts(a, 136) == std::slice::from_raw_parts(b, 136)) as u8 }
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIdentity_SetLocalHost(p: *mut u8) -> u64 {
    if !p.is_null() {
        std::ptr::write_bytes(p, 0, 136);
        *(p as *mut i32) = 3;
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIdentity_IsLocalHost(p: *const u8) -> u8 {
    if p.is_null() { 0 } else { ((*(p as *const i32)) == 3) as u8 }
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIdentity_GetGenericBytes(
    p: *const u8, out: *mut u8, max: i32,
) -> i32 {
    if p.is_null() || out.is_null() || max <= 0 { return 0; }
    let n = 128.min(max as usize);
    std::ptr::copy_nonoverlapping(p.add(8), out, n);
    n as i32
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIdentity_SetGenericBytes(
    p: *mut u8, data: *const u8, len: u32,
) -> u64 {
    if !p.is_null() {
        std::ptr::write_bytes(p, 0, 136);
        *(p as *mut i32) = 2;
        let n = (len as usize).min(128);
        if !data.is_null() { std::ptr::copy_nonoverlapping(data, p.add(8), n); }
        *((p.add(4)) as *mut i32) = n as i32;
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIdentity_GetGenericString(
    p: *const u8, out: *mut c_char, max: i32,
) -> u64 {
    let _ = (p, out, max);
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIdentity_SetGenericString(
    p: *mut u8, s: *const c_char,
) -> u64 {
    let _ = (p, s);
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIdentity_GetIPAddr(
    p: *const u8, out: *mut u8,
) -> u64 {
    if !p.is_null() && !out.is_null() {
        std::ptr::copy_nonoverlapping(p.add(8), out, 20);
        1
    } else { 0 }
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIdentity_SetIPAddr(
    p: *mut u8, addr: *const u8,
) -> u64 {
    if !p.is_null() {
        std::ptr::write_bytes(p, 0, 136);
        *(p as *mut i32) = 4;
        if !addr.is_null() { std::ptr::copy_nonoverlapping(addr, p.add(8), 20); }
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIdentity_GetXboxPairwiseID(
    p: *const u8, out: *mut c_char, max: i32,
) -> u64 {
    let _ = (p, out, max);
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIdentity_SetXboxPairwiseID(
    p: *mut u8, s: *const c_char,
) -> u64 {
    let _ = (p, s);
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIdentity_ParseString(
    p: *mut u8, s: *const c_char,
) -> u8 {
    if p.is_null() { return 0; }
    SteamAPI_SteamNetworkingIdentity_Clear(p);
    let st = cstr(s);
    if let Some(rest) = st.strip_prefix("steamid:") {
        if let Ok(n) = rest.trim().parse::<u64>() {
            SteamAPI_SteamNetworkingIdentity_SetSteamID64(p, n);
            return 1;
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingIdentity_ToString(
    p: *const u8, buf: *mut c_char, bufsize: u32,
) -> u64 {
    if !p.is_null() && !buf.is_null() && bufsize > 24 {
        let s = format!("steamid:{}\0", SteamAPI_SteamNetworkingIdentity_GetSteamID64(p));
        let n = s.len().min(bufsize as usize);
        std::ptr::copy_nonoverlapping(s.as_ptr() as *const c_char, buf, n);
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamNetworkingMessage_t_Release(
    _msg: *mut c_void,
) -> u64 {
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamDatagramHostedAddress_Clear(p: *mut u8) -> u64 {
    if !p.is_null() { std::ptr::write_bytes(p, 0, 16); }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamDatagramHostedAddress_GetPopID(p: *const u8) -> u32 {
    if p.is_null() { 0 } else { *(p as *const u32) }
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_SteamDatagramHostedAddress_SetDevAddress(
    p: *mut u8, ip: u32, port: u16, pop: u32,
) -> u64 {
    if !p.is_null() {
        *(p as *mut u32) = pop;
        *((p.add(4)) as *mut u32) = ip;
        *((p.add(8)) as *mut u16) = port;
    }
    0
}

// servernetadr_t: u16 connPort, u16 queryPort, u32 ip (8 bytes)
#[no_mangle]
pub unsafe extern "system" fn SteamAPI_servernetadr_t_Construct(p: *mut u8) -> u64 {
    if !p.is_null() { std::ptr::write_bytes(p, 0, 8); }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_servernetadr_t_Init(
    p: *mut u8, ip: u32, query: u16, conn: u16,
) -> u64 {
    if !p.is_null() {
        *((p) as *mut u16) = conn;
        *((p.add(2)) as *mut u16) = query;
        *((p.add(4)) as *mut u32) = ip;
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_servernetadr_t_GetIP(p: *const u8) -> u32 {
    if p.is_null() { 0 } else { *((p.add(4)) as *const u32) }
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_servernetadr_t_SetIP(p: *mut u8, ip: u32) -> u64 {
    if !p.is_null() { *((p.add(4)) as *mut u32) = ip; }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_servernetadr_t_GetQueryPort(p: *const u8) -> u16 {
    if p.is_null() { 0 } else { *((p.add(2)) as *const u16) }
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_servernetadr_t_SetQueryPort(p: *mut u8, port: u16) -> u64 {
    if !p.is_null() { *((p.add(2)) as *mut u16) = port; }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_servernetadr_t_GetConnectionPort(p: *const u8) -> u16 {
    if p.is_null() { 0 } else { *(p as *const u16) }
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_servernetadr_t_SetConnectionPort(
    p: *mut u8, port: u16,
) -> u64 {
    if !p.is_null() { *(p as *mut u16) = port; }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_servernetadr_t_GetConnectionAddressString(
    p: *const u8, buf: *mut c_char, bufsize: u32,
) -> u64 {
    if !p.is_null() && !buf.is_null() && bufsize > 24 {
        let ip = SteamAPI_servernetadr_t_GetIP(p);
        let port = SteamAPI_servernetadr_t_GetConnectionPort(p);
        let s = format!("{}.{}.{}.{}:{}\0",
            ip & 0xFF, (ip >> 8) & 0xFF, (ip >> 16) & 0xFF, (ip >> 24) & 0xFF, port);
        let n = s.len().min(bufsize as usize);
        std::ptr::copy_nonoverlapping(s.as_ptr() as *const c_char, buf, n);
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_servernetadr_t_GetQueryAddressString(
    p: *const u8, buf: *mut c_char, bufsize: u32,
) -> u64 {
    if !p.is_null() && !buf.is_null() && bufsize > 24 {
        let ip = SteamAPI_servernetadr_t_GetIP(p);
        let port = SteamAPI_servernetadr_t_GetQueryPort(p);
        let s = format!("{}.{}.{}.{}:{}\0",
            ip & 0xFF, (ip >> 8) & 0xFF, (ip >> 16) & 0xFF, (ip >> 24) & 0xFF, port);
        let n = s.len().min(bufsize as usize);
        std::ptr::copy_nonoverlapping(s.as_ptr() as *const c_char, buf, n);
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_servernetadr_t_IsLessThan(
    a: *const u8, b: *const u8,
) -> u8 {
    if a.is_null() || b.is_null() { 0 }
    else { (SteamAPI_servernetadr_t_GetIP(a) < SteamAPI_servernetadr_t_GetIP(b)) as u8 }
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_servernetadr_t_Assign(dst: *mut u8, src: *const u8) -> u64 {
    if !dst.is_null() && !src.is_null() { std::ptr::copy_nonoverlapping(src, dst, 8); }
    0
}

// gameserveritem_t: m_szServerName at offset 228, 64 bytes (approx layout)
#[no_mangle]
pub unsafe extern "system" fn SteamAPI_gameserveritem_t_Construct(p: *mut u8) -> u64 {
    if !p.is_null() { std::ptr::write_bytes(p, 0, 364); }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_gameserveritem_t_GetName(p: *const u8) -> u64 {
    if p.is_null() { 0 } else { p.add(228) as u64 }
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_gameserveritem_t_SetName(
    p: *mut u8, name: *const c_char,
) -> u64 {
    if !p.is_null() && !name.is_null() {
        let s = cstr(name);
        let bytes = s.as_bytes();
        let n = bytes.len().min(63);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.add(228), n);
        *p.add(228 + n) = 0;
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn SteamAPI_MatchMakingKeyValuePair_t_Construct(p: *mut u8) -> u64 {
    if !p.is_null() { std::ptr::write_bytes(p, 0, 520); }
    0
}
