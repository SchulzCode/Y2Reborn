//! Every value Spotify is likely to change, in one place (spec section 4, rule 3).
//!
//! When something breaks, look here first. Values were verified against
//! librespot commit 939dc5ee (2026-09-11) and the spec, section 3.

use std::time::Duration;

use housecat_protocol as proto;
use housecat_protocol::spotify::clienttoken::data::v0 as ctd;

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Spotify's own desktop client ID. The only client ID whose tokens login5 still
/// accepts for stored-credential logins (since 2026-08-10).
pub const CLIENT_ID_DESKTOP: &str = "65b708073fc0480ea92a077233ca87bd";

/// Wire protocol version sent in the handshake `BuildInfo.version` and the user agent.
pub const SPOTIFY_VERSION: u64 = 124_200_290;

/// Desktop client version string sent to the client-token service.
pub const SPOTIFY_SEMANTIC_VERSION: &str = "1.2.52.442";

/// What we call ourselves: the login `version_string`, the system information
/// string, and the parenthesised part of the user agent.
pub const VERSION_STRING: &str = concat!("housecat-", env!("CARGO_PKG_VERSION"));

// ---------------------------------------------------------------------------
// Endpoints
// ---------------------------------------------------------------------------

pub const APRESOLVE_URL: &str = "https://apresolve.spotify.com/?type=accesspoint&type=spclient";
pub const AP_FALLBACK: &str = "ap.spotify.com:443";
pub const SPCLIENT_FALLBACK: &str = "spclient.wg.spotify.com:443";

pub const CLIENT_TOKEN_URL: &str = "https://clienttoken.spotify.com/v1/clienttoken";
pub const LOGIN5_URL: &str = "https://login5.spotify.com/v3/login";

pub const OAUTH_AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
pub const OAUTH_TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
/// Device authorization flow: the user enters a short code at spotify.com/pair.
pub const OAUTH_DEVICE_AUTHORIZE_URL: &str = "https://accounts.spotify.com/oauth2/device/authorize";
/// Must match the redirect registered for the desktop client ID exactly.
pub const OAUTH_REDIRECT_URI: &str = "http://127.0.0.1:8898/login";
pub const OAUTH_REDIRECT_PORT: u16 = 8898;

/// Scopes requested during OAuth, the list librespot requests. `streaming` is the
/// one that matters for playback.
pub const OAUTH_SCOPES: &[&str] = &[
    "app-remote-control",
    "playlist-modify",
    "playlist-modify-private",
    "playlist-modify-public",
    "playlist-read",
    "playlist-read-collaborative",
    "playlist-read-private",
    "streaming",
    "ugc-image-upload",
    "user-follow-modify",
    "user-follow-read",
    "user-library-modify",
    "user-library-read",
    "user-modify",
    "user-modify-playback-state",
    "user-modify-private",
    "user-personalized",
    "user-read-birthdate",
    "user-read-currently-playing",
    "user-read-email",
    "user-read-play-history",
    "user-read-playback-position",
    "user-read-playback-state",
    "user-read-private",
    "user-read-recently-played",
    "user-top-read",
];

// ---------------------------------------------------------------------------
// Timeouts and limits
// ---------------------------------------------------------------------------

pub const AP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// How long an audio key request may wait for the access point.
pub const AUDIO_KEY_TIMEOUT: Duration = Duration::from_secs(5);
/// How long connect() waits for the ProductInfo and CountryCode packets after login.
pub const PRODUCT_INFO_TIMEOUT: Duration = Duration::from_secs(10);
/// Tokens are refreshed this long before they expire.
pub const TOKEN_EXPIRY_MARGIN: Duration = Duration::from_secs(60);
pub const HASHCASH_TIMEOUT: Duration = Duration::from_secs(5);
pub const HASHCASH_MAX_ATTEMPTS: u32 = 3;
pub const LOGIN5_MAX_ATTEMPTS: u32 = 3;
pub const LOGIN5_RETRY_DELAY: Duration = Duration::from_secs(3);
pub const MAX_ACCESS_POINT_ATTEMPTS: usize = 6;
/// How many bytes of an error body are kept in an error message.
pub const ERROR_BODY_LIMIT: usize = 512;

// Keepalive, librespot's values. The server pings roughly every 60 s; we answer
// 60 s later; the server acks; each expectation carries a 20 s margin.
pub const KEEPALIVE_INITIAL_PING_TIMEOUT: Duration = Duration::from_secs(20);
pub const KEEPALIVE_PING_TIMEOUT: Duration = Duration::from_secs(80);
pub const KEEPALIVE_PONG_DELAY: Duration = Duration::from_secs(60);
pub const KEEPALIVE_PONG_ACK_TIMEOUT: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// Retries, rate limiting, reconnect (spec sections 5.3 and 5.6)
// ---------------------------------------------------------------------------

pub const SPCLIENT_RETRY_ATTEMPTS: u32 = 3;
pub const SPCLIENT_RETRY_BASE_DELAY: Duration = Duration::from_millis(200);
pub const SPCLIENT_RETRY_MAX_DELAY: Duration = Duration::from_secs(2);
/// A 429 whose Retry-After exceeds this is surfaced as `RateLimited` instead of waited out.
pub const RATE_LIMIT_MAX_WAIT: Duration = Duration::from_secs(10);
/// Local request budget per spclient host, librespot's value.
pub const RATE_LIMIT_CALLS: u32 = 300;
pub const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(30);
/// Consecutive transport or 5xx failures before the spclient host is rotated.
pub const UNAVAILABLE_BEFORE_REROUTE: u32 = 3;
pub const RECONNECT_INITIAL_DELAY: Duration = Duration::from_secs(1);
pub const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Cryptography
// ---------------------------------------------------------------------------

/// The 768-bit MODP group from RFC 2409 section 6.1, which is the group Spotify's
/// access points use. Big-endian.
pub const DH_PRIME: [u8; 96] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xc9, 0x0f, 0xda, 0xa2, 0x21, 0x68, 0xc2, 0x34,
    0xc4, 0xc6, 0x62, 0x8b, 0x80, 0xdc, 0x1c, 0xd1, 0x29, 0x02, 0x4e, 0x08, 0x8a, 0x67, 0xcc, 0x74,
    0x02, 0x0b, 0xbe, 0xa6, 0x3b, 0x13, 0x9b, 0x22, 0x51, 0x4a, 0x08, 0x79, 0x8e, 0x34, 0x04, 0xdd,
    0xef, 0x95, 0x19, 0xb3, 0xcd, 0x3a, 0x43, 0x1b, 0x30, 0x2b, 0x0a, 0x6d, 0xf2, 0x5f, 0x14, 0x37,
    0x4f, 0xe1, 0x35, 0x6d, 0x6d, 0x51, 0xc2, 0x45, 0xe4, 0x85, 0xb5, 0x76, 0x62, 0x5e, 0x7e, 0xc6,
    0xf4, 0x4c, 0x42, 0xe9, 0xa6, 0x3a, 0x36, 0x20, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
];
pub const DH_GENERATOR: u32 = 2;
/// Private exponent size in bytes, matching librespot.
pub const DH_PRIVATE_KEY_BYTES: usize = 95;

/// The RSA-2048 modulus of the key the access point signs its DH public key with.
/// Exponent 65537. Copied from librespot; a wrong byte here fails every handshake
/// with a signature error.
pub const SERVER_KEY: [u8; 256] = [
    0xac, 0xe0, 0x46, 0x0b, 0xff, 0xc2, 0x30, 0xaf, 0xf4, 0x6b, 0xfe, 0xc3, 0xbf, 0xbf, 0x86, 0x3d,
    0xa1, 0x91, 0xc6, 0xcc, 0x33, 0x6c, 0x93, 0xa1, 0x4f, 0xb3, 0xb0, 0x16, 0x12, 0xac, 0xac, 0x6a,
    0xf1, 0x80, 0xe7, 0xf6, 0x14, 0xd9, 0x42, 0x9d, 0xbe, 0x2e, 0x34, 0x66, 0x43, 0xe3, 0x62, 0xd2,
    0x32, 0x7a, 0x1a, 0x0d, 0x92, 0x3b, 0xae, 0xdd, 0x14, 0x02, 0xb1, 0x81, 0x55, 0x05, 0x61, 0x04,
    0xd5, 0x2c, 0x96, 0xa4, 0x4c, 0x1e, 0xcc, 0x02, 0x4a, 0xd4, 0xb2, 0x0c, 0x00, 0x1f, 0x17, 0xed,
    0xc2, 0x2f, 0xc4, 0x35, 0x21, 0xc8, 0xf0, 0xcb, 0xae, 0xd2, 0xad, 0xd7, 0x2b, 0x0f, 0x9d, 0xb3,
    0xc5, 0x32, 0x1a, 0x2a, 0xfe, 0x59, 0xf3, 0x5a, 0x0d, 0xac, 0x68, 0xf1, 0xfa, 0x62, 0x1e, 0xfb,
    0x2c, 0x8d, 0x0c, 0xb7, 0x39, 0x2d, 0x92, 0x47, 0xe3, 0xd7, 0x35, 0x1a, 0x6d, 0xbd, 0x24, 0xc2,
    0xae, 0x25, 0x5b, 0x88, 0xff, 0xab, 0x73, 0x29, 0x8a, 0x0b, 0xcc, 0xcd, 0x0c, 0x58, 0x67, 0x31,
    0x89, 0xe8, 0xbd, 0x34, 0x80, 0x78, 0x4a, 0x5f, 0xc9, 0x6b, 0x89, 0x9d, 0x95, 0x6b, 0xfc, 0x86,
    0xd7, 0x4f, 0x33, 0xa6, 0x78, 0x17, 0x96, 0xc9, 0xc3, 0x2d, 0x0d, 0x32, 0xa5, 0xab, 0xcd, 0x05,
    0x27, 0xe2, 0xf7, 0x10, 0xa3, 0x96, 0x13, 0xc4, 0x2f, 0x99, 0xc0, 0x27, 0xbf, 0xed, 0x04, 0x9c,
    0x3c, 0x27, 0x58, 0x04, 0xb6, 0xb2, 0x19, 0xf9, 0xc1, 0x2f, 0x02, 0xe9, 0x48, 0x63, 0xec, 0xa1,
    0xb6, 0x42, 0xa0, 0x9d, 0x48, 0x25, 0xf8, 0xb3, 0x9d, 0xd0, 0xe8, 0x6a, 0xf9, 0x48, 0x4d, 0xa1,
    0xc2, 0xba, 0x86, 0x30, 0x42, 0xea, 0x9d, 0xb3, 0x08, 0x6c, 0x19, 0x0e, 0x48, 0xb3, 0x9d, 0x66,
    0xeb, 0x00, 0x06, 0xa2, 0x5a, 0xee, 0xa1, 0x1b, 0x13, 0x87, 0x3c, 0xd7, 0x19, 0xe6, 0x55, 0xbd,
];
pub const SERVER_KEY_EXPONENT: u32 = 65_537;

// ---------------------------------------------------------------------------
// Audio (spec section 7)
// ---------------------------------------------------------------------------

/// storage-resolve, followed by the audio file id in hex. Answers with CDN URLs.
pub const STORAGE_RESOLVE_PATH: &str = "/storage-resolve/files/audio/interactive/";
/// The fixed IV every audio file is encrypted with (AES-128-CTR, per-file key).
pub const AUDIO_AES_IV: [u8; 16] =
    [0x72, 0xe0, 0x67, 0xfb, 0xdd, 0xcb, 0xcf, 0x77, 0xeb, 0xe8, 0xbc, 0x64, 0x3f, 0x63, 0x0d, 0x93];
/// Spotify's Ogg Vorbis files start with a custom page this long; the real stream follows it.
pub const OGG_PREFIX_LEN: u64 = 0xa7;
/// Offset, inside that page, of the four little-endian f32 normalization values.
pub const OGG_NORMALIZATION_OFFSET: u64 = 144;
/// CDN URLs are treated as expired this long before the expiry they carry.
pub const CDN_URL_EXPIRY_MARGIN: Duration = Duration::from_secs(5 * 60);
/// Size of each range request to the CDN.
pub const CDN_CHUNK_SIZE: usize = 128 * 1024;
/// How long one CDN range request may take.
pub const CDN_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a read waits for bytes that have not arrived before failing with `Timeout`.
pub const CDN_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Bytes inspected to decide whether a file is unencrypted when the audio key is refused.
pub const PLAINTEXT_PROBE_LEN: usize = 4096;

// ---------------------------------------------------------------------------
// Catalog (spec section 6.2): search and the library over private endpoints
// ---------------------------------------------------------------------------

/// The desktop client's GraphQL gateway. Search goes here, never to api.spotify.com.
pub const PATHFINDER_URL: &str = "https://api-partner.spotify.com/pathfinder/v2/query";
/// The persisted-query hash of the desktop client's `searchDesktop` operation. Spotify
/// answers `412 Invalid query hash` when it changes. To refresh it: open the installed
/// client's `%APPDATA%\Spotify\Apps\xpui.spa` (a zip), read `xpui-routes-search.js`, and
/// take the 64 hex digits that follow `searchDesktop`. Taken from client 1.2.57.463.
pub const SEARCH_DESKTOP_QUERY_HASH: &str = "dd1513013a4ab0d9c095eac6b6d292c801bef038e11e06b746385a509be24ab0";
/// The library: a JSON body names the set (`collection`, `artist`, `show`, `listenlater`).
pub const COLLECTION_PATH: &str = "/collection/v2/paging";
/// Items per collection page; 5000 returned a whole 2776-item library in one answer.
pub const COLLECTION_PAGE_LIMIT: u32 = 5000;
/// How long a library snapshot serves page requests before it is fetched again.
pub const LIBRARY_SNAPSHOT_TTL: Duration = Duration::from_secs(30);
/// Ids per extended-metadata request; 200 worked, 100 keeps responses small.
pub const METADATA_BATCH_SIZE: usize = 100;

// ---------------------------------------------------------------------------
// What we claim to be. Spotify rejected an obscure platform enum in September
// 2026, so we always report a mainstream desktop platform, x86_64, chosen only
// by OS family (spec section 5.1, step 3).
// ---------------------------------------------------------------------------

pub(crate) fn platform() -> proto::Platform {
    match std::env::consts::OS {
        "windows" => proto::Platform::Win32X8664,
        "macos" => proto::Platform::OsxX8664,
        _ => proto::Platform::LinuxX8664,
    }
}

pub(crate) fn os() -> proto::Os {
    match std::env::consts::OS {
        "windows" => proto::Os::Windows,
        "macos" => proto::Os::Osx,
        _ => proto::Os::Linux,
    }
}

pub(crate) fn cpu_family() -> proto::CpuFamily {
    proto::CpuFamily::CpuX8664
}

/// The platform token in the user agent, `Spotify/<version> <platform>/0 (<VERSION_STRING>)`.
pub(crate) fn user_agent_platform() -> &'static str {
    match std::env::consts::OS {
        "windows" => "Win32",
        "macos" => "OSX",
        _ => "Linux",
    }
}

/// Platform block for the client-token request. Fixed, plausible desktop values
/// in the shape librespot sends; the service only checks the shape.
pub(crate) fn platform_specific_data() -> ctd::platform_specific_data::Data {
    use ctd::platform_specific_data::Data;
    match std::env::consts::OS {
        "windows" => Data::DesktopWindows(ctd::NativeDesktopWindowsData {
            os_version: 10,
            os_build: 21370,
            platform_id: 2,
            unknown_value_5: 0,
            unknown_value_6: 9,
            image_file_machine: 34404,
            pe_machine: 34404,
            unknown_value_10: true,
        }),
        "macos" => Data::DesktopMacos(ctd::NativeDesktopMacOsData {
            system_version: "14.5".to_string(),
            hw_model: "iMac21,1".to_string(),
            compiled_cpu_type: "x86_64".to_string(),
        }),
        _ => Data::DesktopLinux(ctd::NativeDesktopLinuxData {
            system_name: "Linux".to_string(),
            system_release: "6.6.0".to_string(),
            system_version: "#1 SMP PREEMPT_DYNAMIC".to_string(),
            hardware: "x86_64".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crypto_constants_have_the_right_shape() {
        assert_eq!(DH_PRIME.len(), 96);
        assert_eq!(SERVER_KEY.len(), 256);
        assert_eq!(SERVER_KEY[0], 0xac);
        assert_eq!(SERVER_KEY[255], 0xbd);
        assert_eq!(DH_GENERATOR, 2);
    }

    #[test]
    fn we_always_claim_a_mainstream_x86_64_platform() {
        use housecat_protocol::Platform;
        assert!(matches!(
            platform(),
            Platform::Win32X8664 | Platform::OsxX8664 | Platform::LinuxX8664
        ));
        assert!(["Win32", "OSX", "Linux"].contains(&user_agent_platform()));
    }

    #[test]
    fn scopes_include_streaming() {
        assert!(OAUTH_SCOPES.contains(&"streaming"));
        assert_eq!(OAUTH_SCOPES.len(), 26);
    }
}
