use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::app::AppState;
use super::message::Message;

/// Trait that all TUI components implement.
///
/// Components are responsible for rendering themselves into a buffer area
/// and handling input events. They communicate with the rest of the system
/// by returning `Message` values from input handlers.
pub trait Component {
    /// Render the component into the given area of the buffer.
    fn render(&self, area: Rect, buf: &mut Buffer, state: &AppState);

    /// Handle a keyboard event. Returns a `Message` if the event was consumed
    /// and should trigger a state update, or `None` if the event is ignored.
    fn handle_key(&mut self, _key: KeyEvent, _state: &mut AppState) -> Option<Message> {
        None
    }

    /// Handle a mouse event. Returns a `Message` if the event was consumed.
    fn handle_mouse(&mut self, _event: MouseEvent, _state: &mut AppState) -> Option<Message> {
        None
    }

    /// Whether the component needs to be re-rendered.
    fn is_dirty(&self) -> bool {
        true
    }

    /// Clear the dirty flag after rendering.
    fn clear_dirty(&mut self) {}
}
