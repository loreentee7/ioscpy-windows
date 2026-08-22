//! Turns window events into device input messages. Coordinates are normalized
//! to [0, 1] of the screen so they don't depend on resolution or orientation.

use crate::protocol::MessageType;

/// A control message ready to send, built from window input.
pub struct InputFrame {
    pub msg_type: MessageType,
    pub payload: Vec<u8>,
}

impl InputFrame {
    pub fn new(msg_type: MessageType, payload: Vec<u8>) -> Self {
        Self { msg_type, payload }
    }
}

/// Map a window-pixel mouse position to normalized [0, 1] device coordinates.
/// The frame is aspect-fit (letterboxed) and centered in the window, matching
/// `scale_frame`, so we undo the centering offset and divide by the drawn size.
/// Clicks that land in the black bars clamp to the nearest screen edge.
pub fn map_to_norm(
    mx: f32,
    my: f32,
    win_w: usize,
    win_h: usize,
    frame_w: usize,
    frame_h: usize,
) -> (f32, f32) {
    if frame_w == 0 || frame_h == 0 || win_w == 0 || win_h == 0 {
        return (0.0, 0.0);
    }
    let scale = (win_w as f32 / frame_w as f32).min(win_h as f32 / frame_h as f32);
    let dw = frame_w as f32 * scale;
    let dh = frame_h as f32 * scale;
    let x_off = (win_w as f32 - dw) / 2.0;
    let y_off = (win_h as f32 - dh) / 2.0;
    let nx = ((mx - x_off) / dw).clamp(0.0, 1.0);
    let ny = ((my - y_off) / dh).clamp(0.0, 1.0);
    (nx, ny)
}
