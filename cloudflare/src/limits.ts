// Shared framing contract, tested against Rust's actual protocol constants.
export const FRAME = 64 * 1024;
export const WINDOW = 64 * FRAME;
export const IDLE = 300_000;
// This deployment caps ciphertext buffering at 64 MiB across eight sessions.
export const SESSIONS = 8;
export const AUTH_TIMEOUT_MS = 5_000;
export const CONNECT_TIMEOUT_MS = 10_000;
export const CONNECTION_LIMIT = 512;
export const CONTROL_LIMIT = 256;
export const AUTHENTICATING_PER_IP = 8;
export const ANONYMOUS_PER_TARGET = 4;
export const PROOF_MESSAGE_BYTES = 24 * 1024;
export const CONTROL_MESSAGE_BYTES = 4096;
export const ACK_MESSAGE_BYTES = 128;
