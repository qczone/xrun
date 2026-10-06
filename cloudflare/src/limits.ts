// Shared framing contract, tested against Rust's actual protocol constants.
export const FRAME = 64 * 1024;
export const WINDOW = 64 * FRAME;
export const IDLE = 300_000;
// This deployment caps ciphertext buffering at 64 MiB across eight sessions.
export const SESSIONS = 8;
