#ifndef DESKLINK_SWIFT_H
#define DESKLINK_SWIFT_H

#include <stddef.h>
#include <stdint.h>

#define DESKLINK_PAIRING_INVITE_BYTES 181

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DesklinkHandle DesklinkHandle;
typedef struct DesklinkHostHandle DesklinkHostHandle;

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

DesklinkResult desklink_create(const DesklinkConfig *, DesklinkEventCallback, void *, DesklinkHandle **);
DesklinkResult desklink_identity_verify_key(const uint8_t secret_key[32], uint8_t out_verify_key[32]);
DesklinkResult desklink_start_pairing(DesklinkHandle *, DesklinkPairingInfo *);
DesklinkResult desklink_connect_with_code(DesklinkHandle *, const char *code);
DesklinkResult desklink_connect_secure(DesklinkHandle *, const DesklinkSecureConnectionConfig *);
DesklinkResult desklink_connect_pairing_invite(DesklinkHandle *, const DesklinkPairingInviteConnectionConfig *);
DesklinkResult desklink_accept(DesklinkHandle *);
DesklinkResult desklink_reject(DesklinkHandle *);
DesklinkResult desklink_send_input(DesklinkHandle *, const DesklinkInput *);
DesklinkResult desklink_request_keyframe(DesklinkHandle *);
DesklinkResult desklink_release_all(DesklinkHandle *);
void desklink_destroy(DesklinkHandle *);

typedef enum DesklinkHostEventKind {
    DESKLINK_HOST_EVENT_STATE = 1,
    DESKLINK_HOST_EVENT_ERROR = 2,
    DESKLINK_HOST_EVENT_APPROVAL_REQUESTED = 3,
    DESKLINK_HOST_EVENT_INPUT = 4,
    DESKLINK_HOST_EVENT_KEYFRAME_REQUESTED = 5,
    DESKLINK_HOST_EVENT_RELEASE_ALL = 6,
    DESKLINK_HOST_EVENT_METRICS = 7,
} DesklinkHostEventKind;

typedef enum DesklinkHostState {
    DESKLINK_HOST_CONNECTING = 1,
    DESKLINK_HOST_WAITING_FOR_APPROVAL = 2,
    DESKLINK_HOST_NEGOTIATING_CAPABILITIES = 3,
    DESKLINK_HOST_CONNECTED = 4,
    DESKLINK_HOST_STOPPING = 5,
    DESKLINK_HOST_CLOSED = 6,
} DesklinkHostState;

typedef struct DesklinkHostConfig {
    const char *relay_url;
    const char *server_name;
    uint8_t host_device_id[16];
    uint8_t host_secret_key[32];
    uint32_t log_level;
} DesklinkHostConfig;

typedef struct DesklinkHostInput {
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
} DesklinkHostInput;

typedef struct DesklinkHostMetrics {
    uint64_t sent_video_configs;
    uint64_t sent_video_packets;
    uint64_t received_input_events;
    uint64_t keyframe_requests;
} DesklinkHostMetrics;

typedef struct DesklinkHostEvent {
    DesklinkHostEventKind kind;
    DesklinkHostState state;
    const uint8_t *data;
    size_t data_len;
    uint8_t controller_device_id[16];
    uint8_t controller_verify_key[32];
    const uint8_t *fingerprint;
    size_t fingerprint_len;
    DesklinkHostInput input;
    DesklinkHostMetrics metrics;
} DesklinkHostEvent;

typedef void (*DesklinkHostEventCallback)(void *context, const DesklinkHostEvent *event);

typedef struct DesklinkSavedHostMaterial {
    uint8_t session_id[16];
    uint8_t relay_authentication[32];
    uint8_t host_verify_key[32];
    char server_name[256];
} DesklinkSavedHostMaterial;

DesklinkResult desklink_host_create(const DesklinkHostConfig *, DesklinkHostEventCallback, void *, DesklinkHostHandle **);
DesklinkResult desklink_host_start_pairing(DesklinkHostHandle *, uint8_t *, size_t, size_t *, uint64_t *);
DesklinkResult desklink_host_start_from_invite(DesklinkHostHandle *, const uint8_t *, size_t);
DesklinkResult desklink_host_approve(DesklinkHostHandle *, const uint8_t controller_device_id[16], const uint8_t controller_verify_key[32]);
DesklinkResult desklink_host_reject(DesklinkHostHandle *);
DesklinkResult desklink_host_send_video_config(DesklinkHostHandle *, uint64_t, uint32_t, uint16_t, uint16_t, const uint8_t *, size_t);
DesklinkResult desklink_host_send_video_access_unit(DesklinkHostHandle *, uint64_t, uint64_t, uint32_t, const uint8_t *, size_t);
DesklinkResult desklink_host_send_cursor(DesklinkHostHandle *, uint64_t, const uint8_t *, size_t);
DesklinkResult desklink_host_request_keyframe(DesklinkHostHandle *);
DesklinkResult desklink_host_release_all(DesklinkHostHandle *);
DesklinkResult desklink_host_stop(DesklinkHostHandle *);
void desklink_host_destroy(DesklinkHostHandle *);
DesklinkResult desklink_controller_copy_saved_host_material(DesklinkHandle *, DesklinkSavedHostMaterial *);

#ifdef __cplusplus
}
#endif

#endif
