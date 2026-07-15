#ifndef DESKLINK_H
#define DESKLINK_H

#include <stddef.h>
#include <stdint.h>

#define DESKLINK_PAIRING_INVITE_BYTES 181

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DesklinkHandle DesklinkHandle;

typedef enum DesklinkResult {
    DESKLINK_OK = 0,
    DESKLINK_INVALID_ARGUMENT = 1,
    DESKLINK_INVALID_UTF8 = 2,
    DESKLINK_INVALID_STATE = 3,
    DESKLINK_INTERNAL_ERROR = 4,
} DesklinkResult;

typedef enum DesklinkState {
    DESKLINK_IDLE = 0,
    DESKLINK_CREATING_SESSION = 1,
    DESKLINK_CONNECTING_RELAY = 2,
    DESKLINK_SECURE_HANDSHAKE = 3,
    DESKLINK_WAITING_FOR_APPROVAL = 4,
    DESKLINK_NEGOTIATING_CAPABILITIES = 5,
    DESKLINK_STARTING_VIDEO = 6,
    DESKLINK_CONNECTED = 7,
    DESKLINK_DEGRADED = 8,
    DESKLINK_RECOVERING_VIDEO = 9,
    DESKLINK_RECONNECTING = 10,
    DESKLINK_DISCONNECTING = 11,
    DESKLINK_CLOSED = 12,
} DesklinkState;

typedef enum DesklinkEventKind {
    DESKLINK_EVENT_STATE = 1,
    DESKLINK_EVENT_ERROR = 2,
    DESKLINK_EVENT_PAIRING = 3,
    DESKLINK_EVENT_CONTROL = 4,
    DESKLINK_EVENT_INPUT = 5,
    DESKLINK_EVENT_VIDEO_CONFIG = 6,
    DESKLINK_EVENT_H264_ACCESS_UNIT = 7,
    DESKLINK_EVENT_CURSOR = 8,
    DESKLINK_EVENT_METRICS = 9,
    DESKLINK_EVENT_RELEASE_ALL = 10,
} DesklinkEventKind;

typedef enum DesklinkInputKind {
    DESKLINK_INPUT_MOUSE_MOVE = 1,
    DESKLINK_INPUT_MOUSE_BUTTON = 2,
    DESKLINK_INPUT_KEY = 3,
    DESKLINK_INPUT_MOUSE_WHEEL = 4,
} DesklinkInputKind;

typedef enum DesklinkModifier {
    DESKLINK_MODIFIER_SHIFT = 1,
    DESKLINK_MODIFIER_CONTROL = 2,
    DESKLINK_MODIFIER_ALT = 4,
    DESKLINK_MODIFIER_META = 8,
} DesklinkModifier;

typedef struct DesklinkConfig {
    const char *relay_url;
    uint32_t log_level;
} DesklinkConfig;

typedef struct DesklinkSecureConnectionConfig {
    const char *server_name;
    uint8_t session_id[16];
    uint8_t relay_authentication[32];
    uint8_t controller_device_id[16];
    uint8_t controller_secret_key[32];
    uint8_t host_verify_key[32];
} DesklinkSecureConnectionConfig;

typedef struct DesklinkPairingInviteConnectionConfig {
    const char *server_name;
    const uint8_t *invite;
    size_t invite_len;
    uint8_t controller_device_id[16];
    uint8_t controller_secret_key[32];
} DesklinkPairingInviteConnectionConfig;

typedef struct DesklinkPairingInfo {
    uint8_t session_id[16];
    char code[9];
    uint64_t expires_at_unix_s;
} DesklinkPairingInfo;

typedef struct DesklinkInput {
    DesklinkInputKind kind;
    float x;
    float y;
    int32_t wheel_x;
    int32_t wheel_y;
    uint32_t button;
    uint32_t key_code;
    uint32_t character;
    uint8_t pressed;
    uint8_t modifiers;
} DesklinkInput;

typedef struct DesklinkEvent {
    DesklinkEventKind kind;
    const uint8_t *data;
    size_t data_len;
    uint64_t stream_id;
    uint64_t frame_id;
    uint32_t config_version;
    uint16_t width;
    uint16_t height;
    DesklinkState state;
} DesklinkEvent;

typedef void (*DesklinkEventCallback)(void *context, const DesklinkEvent *event);

DesklinkResult desklink_create(
    const DesklinkConfig *config,
    DesklinkEventCallback callback,
    void *context,
    DesklinkHandle **out_handle
);
DesklinkResult desklink_identity_verify_key(
    const uint8_t secret_key[32],
    uint8_t out_verify_key[32]
);
DesklinkResult desklink_start_pairing(
    DesklinkHandle *handle,
    DesklinkPairingInfo *out_pairing
);
DesklinkResult desklink_connect_with_code(
    DesklinkHandle *handle,
    const char *code
);
DesklinkResult desklink_connect_secure(
    DesklinkHandle *handle,
    const DesklinkSecureConnectionConfig *config
);
DesklinkResult desklink_connect_pairing_invite(
    DesklinkHandle *handle,
    const DesklinkPairingInviteConnectionConfig *config
);
DesklinkResult desklink_accept(DesklinkHandle *handle);
DesklinkResult desklink_reject(DesklinkHandle *handle);
DesklinkResult desklink_send_input(
    DesklinkHandle *handle,
    const DesklinkInput *input
);
DesklinkResult desklink_request_keyframe(DesklinkHandle *handle);
DesklinkResult desklink_release_all(DesklinkHandle *handle);
void desklink_destroy(DesklinkHandle *handle);

#ifdef __cplusplus
}
#endif

#endif
