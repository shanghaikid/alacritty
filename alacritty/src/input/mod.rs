//! Handle input from winit.
//!
//! Certain key combinations should send some escape sequence back to the PTY.
//! In order to figure that out, state about which modifier keys are pressed
//! needs to be tracked. Additionally, we need a bit of a state machine to
//! determine what to do when a non-modifier key is pressed.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::mem;
use std::time::{Duration, Instant};

use log::debug;
use winit::dpi::PhysicalPosition;
use winit::event::{
    ElementState, Modifiers, MouseButton, MouseScrollDelta, Touch as TouchEvent, TouchPhase,
};
#[cfg(target_os = "macos")]
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::ModifiersState;
#[cfg(target_os = "macos")]
use winit::platform::macos::ActiveEventLoopExtMacOS;
use winit::window::CursorIcon;

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction, Point, Side};
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::search::Match;
use alacritty_terminal::term::{ClipboardType, Term, TermMode};
use alacritty_terminal::vi_mode::ViMotion;
use alacritty_terminal::vte::ansi::{ClearMode, Handler};

use crate::clipboard::Clipboard;
#[cfg(target_os = "macos")]
use crate::config::window::Decorations;
use crate::config::{
    Action, BindingMode, MouseAction, MouseEvent, SearchAction, UiConfig, ViAction,
};
use crate::display::hint::HintMatch;
use crate::display::window::{ImeInhibitor, Window};
use crate::display::{Display, SizeInfo};
use crate::event::{
    ClickState, Event, EventType, InlineSearchState, Mouse, TouchPurpose, TouchZoom,
};
use crate::message_bar::{self, Message};
use crate::scheduler::{Scheduler, TimerId, Topic};

pub mod keyboard;

/// Font size change interval in px.
pub const FONT_SIZE_STEP: f32 = 1.;

/// Interval for mouse scrolling during selection outside of the boundaries.
const SELECTION_SCROLLING_INTERVAL: Duration = Duration::from_millis(15);

/// Buffer at the window edge, including fullscreen windows where the pointer cannot leave.
const SELECTION_SCROLLING_EDGE: i32 = 1;

/// Number of pixels for increasing the selection scrolling speed factor by one.
const SELECTION_SCROLLING_STEP: f64 = 20.;

/// Distance before a touch input is considered a drag.
const MAX_TAP_DISTANCE: f64 = 20.;

/// Threshold used for double_click/triple_click.
fn click_threshold() -> Duration {
    #[cfg(target_os = "macos")]
    if let Ok(interval) = Duration::try_from_secs_f64(objc2_app_kit::NSEvent::doubleClickInterval())
    {
        if !interval.is_zero() {
            return interval;
        }
    }

    Duration::from_millis(400)
}

/// Option-drag is the native macOS rectangle gesture; retain Control-drag compatibility.
pub(crate) fn block_selection(modifiers: ModifiersState) -> bool {
    modifiers.control_key() || (cfg!(target_os = "macos") && modifiers.alt_key())
}

/// Keep pointer feedback consistent between input handling and asynchronous hint updates.
pub(crate) fn mouse_cursor_icon(
    modifiers: ModifiersState,
    mouse_mode: bool,
    shift_passthrough: bool,
    hint_highlighted: bool,
    dragging: bool,
) -> CursorIcon {
    let selecting = !mouse_mode || mouse_reporting_override(modifiers, shift_passthrough);
    if selecting && block_selection(modifiers) {
        CursorIcon::Crosshair
    } else if dragging || modifiers.shift_key() {
        // Shift can extend a selection inside tmux even when its mouse events are forwarded.
        CursorIcon::Text
    } else if selecting && hint_highlighted {
        CursorIcon::Pointer
    } else if !selecting {
        CursorIcon::Default
    } else {
        CursorIcon::Text
    }
}

/// Modifiers which keep mouse clicks and selection in Alacritty instead of the PTY.
pub(crate) fn mouse_reporting_override(modifiers: ModifiersState, shift_passthrough: bool) -> bool {
    (modifiers.shift_key() && !shift_passthrough)
        || (cfg!(target_os = "macos") && modifiers.super_key())
}

/// Processes input from winit.
///
/// An escape sequence may be emitted in case specific keys or key combinations
/// are activated.
pub struct Processor<T: EventListener, A: ActionContext<T>> {
    pub ctx: A,
    _phantom: PhantomData<T>,
}

pub trait ActionContext<T: EventListener> {
    fn write_to_pty<B: Into<Cow<'static, [u8]>>>(&self, _data: B) {}
    fn mark_dirty(&mut self) {}
    fn size_info(&self) -> SizeInfo;
    fn copy_selection(&mut self, _ty: ClipboardType) {}
    fn start_selection(&mut self, _ty: SelectionType, _point: Point, _side: Side) {}
    fn toggle_selection(&mut self, _ty: SelectionType, _point: Point, _side: Side) {}
    fn update_selection(&mut self, _point: Point, _side: Side) {}
    fn clear_selection(&mut self) {}
    fn selection_is_empty(&self) -> bool;
    fn mouse_mut(&mut self) -> &mut Mouse;
    fn mouse(&self) -> &Mouse;
    fn touch_purpose(&mut self) -> &mut TouchPurpose;
    fn modifiers(&mut self) -> &mut Modifiers;
    fn scroll(&mut self, _scroll: Scroll) {}
    fn window(&mut self) -> &mut Window;
    fn display(&mut self) -> &mut Display;
    fn terminal(&self) -> &Term<T>;
    fn terminal_mut(&mut self) -> &mut Term<T>;
    fn spawn_new_instance(&mut self) {}
    #[cfg(target_os = "macos")]
    fn create_new_window(&mut self, _tabbing_id: Option<String>) {}
    #[cfg(not(target_os = "macos"))]
    fn create_new_window(&mut self) {}
    fn change_font_size(&mut self, _delta: f32) {}
    fn reset_font_size(&mut self) {}
    fn pop_message(&mut self) {}
    fn message(&self) -> Option<&Message>;
    fn config(&self) -> &UiConfig;
    #[cfg(target_os = "macos")]
    fn event_loop(&self) -> &ActiveEventLoop;
    fn mouse_mode(&self) -> bool;
    fn clipboard_mut(&mut self) -> &mut Clipboard;
    fn scheduler_mut(&mut self) -> &mut Scheduler;
    fn start_search(&mut self, _direction: Direction) {}
    fn start_seeded_search(&mut self, _direction: Direction, _text: String) {}
    fn confirm_search(&mut self) {}
    fn cancel_search(&mut self) {}
    fn search_input(&mut self, _c: char) {}
    fn search_pop_word(&mut self) {}
    fn search_history_previous(&mut self) {}
    fn search_history_next(&mut self) {}
    fn search_next(&mut self, origin: Point, direction: Direction, side: Side) -> Option<Match>;
    fn advance_search_origin(&mut self, _direction: Direction) {}
    fn search_direction(&self) -> Direction;
    fn search_active(&self) -> bool;
    fn on_typing_start(&mut self) {}
    fn toggle_vi_mode(&mut self) {}
    fn inline_search_state(&mut self) -> &mut InlineSearchState;
    fn start_inline_search(&mut self, _direction: Direction, _stop_short: bool) {}
    fn inline_search_next(&mut self) {}
    fn inline_search_input(&mut self, _text: &str) {}
    fn inline_search_previous(&mut self) {}
    fn hint_input(&mut self, _character: char) {}
    fn trigger_hint(&mut self, _hint: &HintMatch) {}
    fn expand_selection(&mut self) {}
    fn semantic_word(&self, point: Point) -> String;
    fn on_terminal_input_start(&mut self) {}
    fn paste(&mut self, _text: &str, _bracketed: bool) {}
    fn spawn_daemon<I, S>(&self, _program: &str, _args: I)
    where
        I: IntoIterator<Item = S> + Debug + Copy,
        S: AsRef<OsStr>,
    {
    }
}

impl Action {
    fn toggle_selection<T, A>(ctx: &mut A, ty: SelectionType)
    where
        A: ActionContext<T>,
        T: EventListener,
    {
        ctx.toggle_selection(ty, ctx.terminal().vi_mode_cursor.point, Side::Left);

        // Make sure initial selection is not empty.
        if let Some(selection) = &mut ctx.terminal_mut().selection {
            selection.include_all();
        }
    }
}

trait Execute<T: EventListener> {
    fn execute<A: ActionContext<T>>(&self, ctx: &mut A);
}

impl<T: EventListener> Execute<T> for Action {
    #[inline]
    fn execute<A: ActionContext<T>>(&self, ctx: &mut A) {
        match self {
            Action::Esc(s) => ctx.paste(s, false),
            Action::Command(program) => ctx.spawn_daemon(program.program(), program.args()),
            Action::Hint(hint) => {
                ctx.display().hint_state.start(hint.clone());
                ctx.mark_dirty();
            },
            Action::ToggleViMode => {
                ctx.on_typing_start();
                ctx.toggle_vi_mode()
            },
            action @ (Action::ViMotion(_) | Action::Vi(_))
                if !ctx.terminal().mode().contains(TermMode::VI) =>
            {
                debug!("Ignoring {action:?}: Vi mode inactive");
            },
            Action::ViMotion(motion) => {
                ctx.on_typing_start();
                ctx.terminal_mut().vi_motion(*motion);
                ctx.mark_dirty();
            },
            Action::Vi(ViAction::ToggleNormalSelection) => {
                Self::toggle_selection(ctx, SelectionType::Simple);
            },
            Action::Vi(ViAction::ToggleLineSelection) => {
                Self::toggle_selection(ctx, SelectionType::Lines);
            },
            Action::Vi(ViAction::ToggleBlockSelection) => {
                Self::toggle_selection(ctx, SelectionType::Block);
            },
            Action::Vi(ViAction::ToggleSemanticSelection) => {
                Self::toggle_selection(ctx, SelectionType::Semantic);
            },
            Action::Vi(ViAction::Open) => {
                let hint = ctx.display().vi_highlighted_hint.take();
                if let Some(hint) = &hint {
                    ctx.mouse_mut().block_hint_launcher = false;
                    ctx.trigger_hint(hint);
                }
                ctx.display().vi_highlighted_hint = hint;
            },
            Action::Vi(ViAction::SearchNext) => {
                ctx.on_typing_start();

                let terminal = ctx.terminal();
                let direction = ctx.search_direction();
                let vi_point = terminal.vi_mode_cursor.point;
                let origin = match direction {
                    Direction::Right => vi_point.add(terminal, Boundary::None, 1),
                    Direction::Left => vi_point.sub(terminal, Boundary::None, 1),
                };

                if let Some(regex_match) = ctx.search_next(origin, direction, Side::Left) {
                    ctx.terminal_mut().vi_goto_point(*regex_match.start());
                    ctx.mark_dirty();
                }
            },
            Action::Vi(ViAction::SearchPrevious) => {
                ctx.on_typing_start();

                let terminal = ctx.terminal();
                let direction = ctx.search_direction().opposite();
                let vi_point = terminal.vi_mode_cursor.point;
                let origin = match direction {
                    Direction::Right => vi_point.add(terminal, Boundary::None, 1),
                    Direction::Left => vi_point.sub(terminal, Boundary::None, 1),
                };

                if let Some(regex_match) = ctx.search_next(origin, direction, Side::Left) {
                    ctx.terminal_mut().vi_goto_point(*regex_match.start());
                    ctx.mark_dirty();
                }
            },
            Action::Vi(ViAction::SearchStart) => {
                let terminal = ctx.terminal();
                let origin = terminal.vi_mode_cursor.point.sub(terminal, Boundary::None, 1);

                if let Some(regex_match) = ctx.search_next(origin, Direction::Left, Side::Left) {
                    ctx.terminal_mut().vi_goto_point(*regex_match.start());
                    ctx.mark_dirty();
                }
            },
            Action::Vi(ViAction::SearchEnd) => {
                let terminal = ctx.terminal();
                let origin = terminal.vi_mode_cursor.point.add(terminal, Boundary::None, 1);

                if let Some(regex_match) = ctx.search_next(origin, Direction::Right, Side::Right) {
                    ctx.terminal_mut().vi_goto_point(*regex_match.end());
                    ctx.mark_dirty();
                }
            },
            Action::Vi(ViAction::CenterAroundViCursor) => {
                let term = ctx.terminal();
                let display_offset = term.grid().display_offset() as i32;
                let target = -display_offset + term.screen_lines() as i32 / 2 - 1;
                let line = term.vi_mode_cursor.point.line;
                let scroll_lines = target - line.0;

                ctx.scroll(Scroll::Delta(scroll_lines));
            },
            Action::Vi(ViAction::InlineSearchForward) => {
                ctx.start_inline_search(Direction::Right, false)
            },
            Action::Vi(ViAction::InlineSearchBackward) => {
                ctx.start_inline_search(Direction::Left, false)
            },
            Action::Vi(ViAction::InlineSearchForwardShort) => {
                ctx.start_inline_search(Direction::Right, true)
            },
            Action::Vi(ViAction::InlineSearchBackwardShort) => {
                ctx.start_inline_search(Direction::Left, true)
            },
            Action::Vi(ViAction::InlineSearchNext) => ctx.inline_search_next(),
            Action::Vi(ViAction::InlineSearchPrevious) => ctx.inline_search_previous(),
            Action::Vi(ViAction::SemanticSearchForward | ViAction::SemanticSearchBackward) => {
                let seed_text = match ctx.terminal().selection_to_string() {
                    Some(selection) if !selection.is_empty() => selection,
                    // Get semantic word at the vi cursor position.
                    _ => ctx.semantic_word(ctx.terminal().vi_mode_cursor.point),
                };

                if !seed_text.is_empty() {
                    let direction = match self {
                        Action::Vi(ViAction::SemanticSearchForward) => Direction::Right,
                        _ => Direction::Left,
                    };
                    ctx.start_seeded_search(direction, seed_text);
                }
            },
            action @ Action::Search(_) if !ctx.search_active() => {
                debug!("Ignoring {action:?}: Search mode inactive");
            },
            Action::Search(SearchAction::SearchFocusNext) => {
                ctx.advance_search_origin(ctx.search_direction());
            },
            Action::Search(SearchAction::SearchFocusPrevious) => {
                let direction = ctx.search_direction().opposite();
                ctx.advance_search_origin(direction);
            },
            Action::Search(SearchAction::SearchConfirm) => ctx.confirm_search(),
            Action::Search(SearchAction::SearchCancel) => ctx.cancel_search(),
            Action::Search(SearchAction::SearchClear) => {
                let direction = ctx.search_direction();
                ctx.cancel_search();
                ctx.start_search(direction);
            },
            Action::Search(SearchAction::SearchDeleteWord) => ctx.search_pop_word(),
            Action::Search(SearchAction::SearchHistoryPrevious) => ctx.search_history_previous(),
            Action::Search(SearchAction::SearchHistoryNext) => ctx.search_history_next(),
            Action::Mouse(MouseAction::ExpandSelection) => ctx.expand_selection(),
            Action::SearchForward => ctx.start_search(Direction::Right),
            Action::SearchBackward => ctx.start_search(Direction::Left),
            Action::Copy => ctx.copy_selection(ClipboardType::Clipboard),
            #[cfg(not(any(target_os = "macos", windows)))]
            Action::CopySelection => ctx.copy_selection(ClipboardType::Selection),
            Action::ClearSelection => ctx.clear_selection(),
            Action::Paste => {
                let allow_images = !ctx.search_active() && !ctx.inline_search_state().char_pending;
                let text = ctx.clipboard_mut().load_for_paste(allow_images);
                ctx.paste(&text, true);
            },
            Action::PasteSelection => {
                let text = ctx.clipboard_mut().load(ClipboardType::Selection);
                ctx.paste(&text, true);
            },
            Action::ToggleFullscreen => ctx.window().toggle_fullscreen(),
            Action::ToggleMaximized => ctx.window().toggle_maximized(),
            #[cfg(target_os = "macos")]
            Action::ToggleSimpleFullscreen => ctx.window().toggle_simple_fullscreen(),
            #[cfg(target_os = "macos")]
            Action::Hide => ctx.event_loop().hide_application(),
            #[cfg(target_os = "macos")]
            Action::HideOtherApplications => ctx.event_loop().hide_other_applications(),
            #[cfg(not(target_os = "macos"))]
            Action::Hide => ctx.window().set_visible(false),
            Action::Minimize => ctx.window().set_minimized(true),
            Action::Quit => {
                ctx.window().hold = false;
                ctx.terminal_mut().exit();
            },
            Action::IncreaseFontSize => ctx.change_font_size(FONT_SIZE_STEP),
            Action::DecreaseFontSize => ctx.change_font_size(-FONT_SIZE_STEP),
            Action::ResetFontSize => ctx.reset_font_size(),
            Action::ScrollPageUp
            | Action::ScrollPageDown
            | Action::ScrollHalfPageUp
            | Action::ScrollHalfPageDown => {
                // Move vi mode cursor.
                let term = ctx.terminal_mut();
                let (scroll, amount) = match self {
                    Action::ScrollPageUp => (Scroll::PageUp, term.screen_lines() as i32),
                    Action::ScrollPageDown => (Scroll::PageDown, -(term.screen_lines() as i32)),
                    Action::ScrollHalfPageUp => {
                        let amount = term.screen_lines() as i32 / 2;
                        (Scroll::Delta(amount), amount)
                    },
                    Action::ScrollHalfPageDown => {
                        let amount = -(term.screen_lines() as i32 / 2);
                        (Scroll::Delta(amount), amount)
                    },
                    _ => unreachable!(),
                };

                let old_vi_cursor = term.vi_mode_cursor;
                term.vi_mode_cursor = term.vi_mode_cursor.scroll(term, amount);
                if old_vi_cursor != term.vi_mode_cursor {
                    ctx.mark_dirty();
                }

                ctx.scroll(scroll);
            },
            Action::ScrollLineUp => ctx.scroll(Scroll::Delta(1)),
            Action::ScrollLineDown => ctx.scroll(Scroll::Delta(-1)),
            Action::ScrollToPreviousPrompt | Action::ScrollToNextPrompt => {
                if let Some(delta) = ctx
                    .terminal_mut()
                    .shell_prompt_scroll(matches!(self, Action::ScrollToPreviousPrompt))
                {
                    ctx.scroll(Scroll::Delta(delta));
                    ctx.clear_selection();
                    if let Some((start, end)) = ctx.terminal().command_output_range() {
                        ctx.start_selection(SelectionType::Simple, start, Side::Left);
                        ctx.update_selection(end, Side::Right);
                    }
                    ctx.mark_dirty();
                }
            },
            Action::CopyCommandOutput => {
                if let Some(text) = ctx.terminal().command_output_to_string() {
                    ctx.clipboard_mut().store(ClipboardType::Clipboard, text);
                }
            },
            Action::SelectCommandOutput => {
                if let Some((start, end)) = ctx.terminal().command_output_range() {
                    ctx.start_selection(SelectionType::Simple, start, Side::Left);
                    ctx.update_selection(end, Side::Right);
                    ctx.mark_dirty();
                }
            },
            Action::ScrollToTop => {
                ctx.scroll(Scroll::Top);

                // Move vi mode cursor.
                let topmost_line = ctx.terminal().topmost_line();
                ctx.terminal_mut().vi_mode_cursor.point.line = topmost_line;
                ctx.terminal_mut().vi_motion(ViMotion::FirstOccupied);
                ctx.mark_dirty();
            },
            Action::ScrollToBottom => {
                ctx.scroll(Scroll::Bottom);

                // Move vi mode cursor.
                let term = ctx.terminal_mut();
                term.vi_mode_cursor.point.line = term.bottommost_line();

                // Move to beginning twice, to always jump across linewraps.
                term.vi_motion(ViMotion::FirstOccupied);
                term.vi_motion(ViMotion::FirstOccupied);
                ctx.mark_dirty();
            },
            Action::ClearHistory => ctx.terminal_mut().clear_screen(ClearMode::Saved),
            Action::ClearLogNotice => ctx.pop_message(),
            #[cfg(not(target_os = "macos"))]
            Action::CreateNewWindow => ctx.create_new_window(),
            Action::SpawnNewInstance => ctx.spawn_new_instance(),
            #[cfg(target_os = "macos")]
            Action::CreateNewWindow => ctx.create_new_window(None),
            #[cfg(target_os = "macos")]
            Action::CreateNewTab => {
                // Tabs on macOS are not possible without decorations.
                if ctx.config().window.decorations != Decorations::None {
                    let tabbing_id = Some(ctx.window().tabbing_id());
                    ctx.create_new_window(tabbing_id);
                }
            },
            #[cfg(target_os = "macos")]
            Action::SelectNextTab => ctx.window().select_next_tab(),
            #[cfg(target_os = "macos")]
            Action::SelectPreviousTab => ctx.window().select_previous_tab(),
            #[cfg(target_os = "macos")]
            Action::SelectTab1 => ctx.window().select_tab_at_index(0),
            #[cfg(target_os = "macos")]
            Action::SelectTab2 => ctx.window().select_tab_at_index(1),
            #[cfg(target_os = "macos")]
            Action::SelectTab3 => ctx.window().select_tab_at_index(2),
            #[cfg(target_os = "macos")]
            Action::SelectTab4 => ctx.window().select_tab_at_index(3),
            #[cfg(target_os = "macos")]
            Action::SelectTab5 => ctx.window().select_tab_at_index(4),
            #[cfg(target_os = "macos")]
            Action::SelectTab6 => ctx.window().select_tab_at_index(5),
            #[cfg(target_os = "macos")]
            Action::SelectTab7 => ctx.window().select_tab_at_index(6),
            #[cfg(target_os = "macos")]
            Action::SelectTab8 => ctx.window().select_tab_at_index(7),
            #[cfg(target_os = "macos")]
            Action::SelectTab9 => ctx.window().select_tab_at_index(8),
            #[cfg(target_os = "macos")]
            Action::SelectLastTab => ctx.window().select_last_tab(),
            _ => (),
        }
    }
}

impl<T: EventListener, A: ActionContext<T>> Processor<T, A> {
    pub fn new(ctx: A) -> Self {
        Self { ctx, _phantom: Default::default() }
    }

    /// Whether the current button gesture belongs to terminal selection rather than the PTY.
    fn native_selection(&mut self) -> bool {
        (self.ctx.mouse().left_button_state == ElementState::Pressed
            || self.ctx.mouse().right_button_state == ElementState::Pressed)
            && self.ctx.mouse().click_state != ClickState::None
            && (mouse_reporting_override(
                self.ctx.modifiers().state(),
                self.ctx.config().mouse.shift_passthrough,
            ) || !self.ctx.mouse_mode())
    }

    /// Revalidate queued autoscroll ticks after button, modifier, focus, or mode changes.
    pub fn scroll_selection(&mut self, delta: i32) -> bool {
        if !self.native_selection() || self.ctx.selection_is_empty() {
            return false;
        }
        self.ctx.scroll(Scroll::Delta(delta));
        true
    }

    #[inline]
    pub fn mouse_moved(&mut self, position: PhysicalPosition<f64>) {
        let size_info = self.ctx.size_info();

        let (x, y): (i32, i32) = position.into();

        let lmb_pressed = self.ctx.mouse().left_button_state == ElementState::Pressed;
        let display_offset = self.ctx.terminal().grid().display_offset();
        let old_point = self.ctx.mouse().point(&size_info, display_offset);

        let x = x.clamp(0, size_info.width() as i32 - 1) as usize;
        let y = y.clamp(0, size_info.height() as i32 - 1) as usize;
        self.ctx.mouse_mut().x = x;
        self.ctx.mouse_mut().y = y;

        let inside_text_area = size_info.contains_point(x, y);
        let cell_side = self.cell_side(x);

        let point = self.ctx.mouse().point(&size_info, display_offset);
        let cell_changed = old_point != point;
        let native_selection = self.native_selection();

        // Application-owned drags (including tmux copy mode) also select text. Keep ordinary
        // clicks on its tab bar as arrows, and switch to an I-beam only once dragging begins.
        if lmb_pressed && cell_changed {
            self.ctx.mouse_mut().click_dragged = true;
        }

        // Edge motion can stay in the same cell (especially with large padding).
        if native_selection && !self.ctx.selection_is_empty() {
            self.update_selection_scrolling(position.y as i32);
        }

        // If the mouse hasn't changed cells, do nothing.
        if !cell_changed
            && self.ctx.mouse().cell_side == cell_side
            && self.ctx.mouse().inside_text_area == inside_text_area
        {
            return;
        }

        self.ctx.mouse_mut().inside_text_area = inside_text_area;
        self.ctx.mouse_mut().cell_side = cell_side;

        // Prompt hint highlight update.
        self.ctx.mouse_mut().hint_highlight_dirty = true;

        // Don't launch URLs if mouse has moved.
        self.ctx.mouse_mut().block_hint_launcher = true;

        if native_selection {
            // Option/Control can change a cell drag's shape. Shift continues the existing
            // anchor's selection type, including a rectangle whose modifier was released.
            let modifiers = self.ctx.modifiers().state();
            if !modifiers.shift_key() {
                let rectangle = block_selection(modifiers);
                if let Some(selection) = self.ctx.terminal_mut().selection.as_mut() {
                    if matches!(selection.ty, SelectionType::Simple | SelectionType::Block) {
                        selection.ty =
                            if rectangle { SelectionType::Block } else { SelectionType::Simple };
                    }
                }
            }
            self.ctx.update_selection(point, cell_side);
            // A drag cannot be continued as a double click, even after returning to its origin.
            if lmb_pressed && (cell_changed || !self.ctx.selection_is_empty()) {
                self.ctx.mouse_mut().click_dragged = true;
            }
            // Check after updating the selection so the first movement can start scrolling.
            if !self.ctx.selection_is_empty() {
                self.update_selection_scrolling(position.y as i32);
            }
        } else if cell_changed
            && !mouse_reporting_override(
                self.ctx.modifiers().state(),
                self.ctx.config().mouse.shift_passthrough,
            )
            && self.ctx.terminal().mode().intersects(TermMode::MOUSE_MOTION | TermMode::MOUSE_DRAG)
        {
            if lmb_pressed {
                self.mouse_report(32, ElementState::Pressed);
            } else if self.ctx.mouse().middle_button_state == ElementState::Pressed {
                self.mouse_report(33, ElementState::Pressed);
            } else if self.ctx.mouse().right_button_state == ElementState::Pressed {
                self.mouse_report(34, ElementState::Pressed);
            } else if self.ctx.terminal().mode().contains(TermMode::MOUSE_MOTION) {
                self.mouse_report(35, ElementState::Pressed);
            }
        }

        // Resolve the pointer after selection updates so a link cannot leave a hand cursor behind.
        let mouse_state = self.cursor_state();
        self.ctx.window().set_mouse_cursor(mouse_state);
    }

    /// Check which side of a cell an X coordinate lies on.
    fn cell_side(&self, x: usize) -> Side {
        let size_info = self.ctx.size_info();

        let cell_width = size_info.cell_width() as usize;
        let grid_x = x.saturating_sub(size_info.padding_x() as usize);
        // Ghostty includes a cell once the pointer crosses 60% of its width.
        let threshold = (cell_width as f64 * 0.6).round() as usize;

        if grid_x >= size_info.columns() * cell_width || grid_x % cell_width >= threshold {
            Side::Right
        } else {
            Side::Left
        }
    }

    fn mouse_report(&mut self, button: u8, state: ElementState) {
        let display_offset = self.ctx.terminal().grid().display_offset();
        let point = self.ctx.mouse().point(&self.ctx.size_info(), display_offset);

        // Assure the mouse point is not in the scrollback.
        if point.line < 0 {
            return;
        }

        // Calculate modifiers value.
        let mut mods = 0;
        let modifiers = self.ctx.modifiers().state();
        if modifiers.shift_key() {
            mods += 4;
        }
        if modifiers.alt_key() {
            mods += 8;
        }
        if modifiers.control_key() {
            mods += 16;
        }

        // Report mouse events.
        if self.ctx.terminal().mode().contains(TermMode::SGR_MOUSE) {
            self.sgr_mouse_report(point, button + mods, state);
        } else if let ElementState::Released = state {
            self.normal_mouse_report(point, 3 + mods);
        } else {
            self.normal_mouse_report(point, button + mods);
        }
    }

    fn normal_mouse_report(&mut self, point: Point, button: u8) {
        let Point { line, column } = point;
        let utf8 = self.ctx.terminal().mode().contains(TermMode::UTF8_MOUSE);

        let max_point = if utf8 { 2015 } else { 223 };

        if line >= max_point || column >= max_point {
            return;
        }

        let mut msg = vec![b'\x1b', b'[', b'M', 32 + button];

        let mouse_pos_encode = |pos: usize| -> Vec<u8> {
            let pos = 32 + 1 + pos;
            let first = 0xC0 + pos / 64;
            let second = 0x80 + (pos & 63);
            vec![first as u8, second as u8]
        };

        if utf8 && column >= Column(95) {
            msg.append(&mut mouse_pos_encode(column.0));
        } else {
            msg.push(32 + 1 + column.0 as u8);
        }

        if utf8 && line >= 95 {
            msg.append(&mut mouse_pos_encode(line.0 as usize));
        } else {
            msg.push(32 + 1 + line.0 as u8);
        }

        self.ctx.write_to_pty(msg);
    }

    fn sgr_mouse_report(&mut self, point: Point, button: u8, state: ElementState) {
        let c = match state {
            ElementState::Pressed => 'M',
            ElementState::Released => 'm',
        };

        let msg = format!("\x1b[<{};{};{}{}", button, point.column + 1, point.line + 1, c);
        self.ctx.write_to_pty(msg.into_bytes());
    }

    fn on_mouse_press(&mut self, button: MouseButton) {
        // Handle mouse mode.
        if !mouse_reporting_override(
            self.ctx.modifiers().state(),
            self.ctx.config().mouse.shift_passthrough,
        ) && self.ctx.mouse_mode()
        {
            self.ctx.mouse_mut().click_state = ClickState::None;
            self.ctx.mouse_mut().click_dragged = false;

            let code = match button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                MouseButton::Right => 2,
                // Can't properly report more than three buttons..
                MouseButton::Back | MouseButton::Forward | MouseButton::Other(_) => return,
            };

            self.mouse_report(code, ElementState::Pressed);
        } else {
            // Calculate time since the last click to handle double/triple clicks.
            let now = Instant::now();
            let elapsed = now - self.ctx.mouse().last_click_timestamp;
            self.ctx.mouse_mut().last_click_timestamp = now;
            let mouse = self.ctx.mouse();
            let distance = (mouse.x.abs_diff(mouse.last_click_position.0) as f64)
                .hypot(mouse.y.abs_diff(mouse.last_click_position.1) as f64);
            let repeat = elapsed < click_threshold()
                && distance <= self.ctx.size_info().cell_width() as f64
                && !mouse.click_dragged;
            let rectangle = block_selection(self.ctx.modifiers().state());

            // Update multi-click state.
            self.ctx.mouse_mut().click_state = match self.ctx.mouse().click_state {
                // Reset click state if button has changed.
                _ if button != self.ctx.mouse().last_click_button || rectangle => {
                    self.ctx.mouse_mut().last_click_button = button;
                    ClickState::Click
                },
                ClickState::Click if repeat => ClickState::DoubleClick,
                ClickState::DoubleClick | ClickState::TripleClick if repeat => {
                    ClickState::TripleClick
                },
                _ => ClickState::Click,
            };
            if self.ctx.mouse().click_state == ClickState::Click {
                self.ctx.mouse_mut().last_click_position = (self.ctx.mouse().x, self.ctx.mouse().y);
            }
            self.ctx.mouse_mut().click_dragged = false;

            // Load mouse point, treating message bar and padding as the closest cell.
            let display_offset = self.ctx.terminal().grid().display_offset();
            let point = self.ctx.mouse().point(&self.ctx.size_info(), display_offset);

            if let MouseButton::Left = button {
                self.on_left_click(point)
            }
        }
    }

    /// Handle left click selection and vi mode cursor movement.
    fn on_left_click(&mut self, point: Point) {
        let side = self.ctx.mouse().cell_side;
        let rectangle = block_selection(self.ctx.modifiers().state());

        // Shift-click moves only the active endpoint, including when the first
        // click created an empty anchor. Keep the original selection type and
        // grid coordinates so this also works after scrolling through history.
        if self.ctx.modifiers().state().shift_key() && self.ctx.terminal().selection.is_some() {
            self.ctx.mouse_mut().block_hint_launcher = true;
            self.ctx.mouse_mut().click_state = ClickState::Click;
            self.ctx.mouse_mut().click_dragged = true;
            self.ctx.update_selection(point, side);
        } else {
            self.start_click_selection(point, side, rectangle);
        }

        // Move vi mode cursor to mouse click position.
        if self.ctx.terminal().mode().contains(TermMode::VI) && !self.ctx.search_active() {
            self.ctx.terminal_mut().vi_mode_cursor.point = point;
            self.ctx.mark_dirty();
        }
    }

    /// Start a new selection for an unmodified click or a click without an anchor.
    fn start_click_selection(&mut self, point: Point, side: Side, rectangle: bool) {
        match self.ctx.mouse().click_state {
            ClickState::Click => {
                // Don't launch URLs if this click cleared the selection.
                self.ctx.mouse_mut().block_hint_launcher =
                    rectangle || !self.ctx.selection_is_empty();

                self.ctx.clear_selection();

                // Start new empty selection.
                if rectangle {
                    self.ctx.start_selection(SelectionType::Block, point, side);
                } else {
                    self.ctx.start_selection(SelectionType::Simple, point, side);
                }
            },
            ClickState::DoubleClick if !rectangle => {
                self.ctx.mouse_mut().block_hint_launcher = true;
                self.ctx.start_selection(SelectionType::Semantic, point, side);
            },
            ClickState::TripleClick if !rectangle => {
                self.ctx.mouse_mut().block_hint_launcher = true;
                self.ctx.start_selection(SelectionType::Lines, point, side);
            },
            _ => (),
        };
    }

    fn on_mouse_release(&mut self, button: MouseButton) {
        // The override modifier may have been released before the button. Always finish a
        // native drag's timer before handing this release back to the application.
        if self.ctx.mouse().click_state != ClickState::None {
            let timer_id = TimerId::new(Topic::SelectionScrolling, self.ctx.window().id());
            self.ctx.scheduler_mut().unschedule(timer_id);
        }

        if !mouse_reporting_override(
            self.ctx.modifiers().state(),
            self.ctx.config().mouse.shift_passthrough,
        ) && self.ctx.mouse_mode()
        {
            let code = match button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                MouseButton::Right => 2,
                // Can't properly report more than three buttons.
                MouseButton::Back | MouseButton::Forward | MouseButton::Other(_) => return,
            };
            self.mouse_report(code, ElementState::Released);
            return;
        }

        // Trigger hints highlighted by the mouse.
        let hint = self.ctx.display().highlighted_hint.take();
        if let Some(hint) = hint.as_ref().filter(|_| button == MouseButton::Left) {
            self.ctx.trigger_hint(hint);
        }
        self.ctx.display().highlighted_hint = hint;

        if let MouseButton::Left | MouseButton::Right = button {
            // Copy selection on release, to prevent flooding the display server.
            self.ctx.copy_selection(ClipboardType::Selection);
        }
    }

    pub fn mouse_wheel_input(&mut self, delta: MouseScrollDelta, _phase: TouchPhase) {
        let multiplier = self.ctx.config().scrolling.multiplier;
        match delta {
            MouseScrollDelta::LineDelta(columns, lines) => {
                let new_scroll_px_x = columns * self.ctx.size_info().cell_width();
                let new_scroll_px_y = lines * self.ctx.size_info().cell_height();
                self.scroll_terminal(
                    new_scroll_px_x as f64,
                    new_scroll_px_y as f64,
                    multiplier as f64,
                );
            },
            MouseScrollDelta::PixelDelta(mut lpos) => {
                // Started and Ended events can carry motion too. In particular,
                // macOS reports the first momentum delta with Started.
                // Keep the fractional remainder across phases as well: resetting
                // it at that boundary loses part of the same physical gesture.
                if lpos.x == 0. && lpos.y == 0. {
                    return;
                }

                // When the angle between (x, 0) and (x, y) is lower than ~25 degrees
                // (cosine is larger that 0.9) we consider this scrolling as horizontal.
                if lpos.x.abs() / lpos.x.hypot(lpos.y) > 0.9 {
                    lpos.y = 0.;
                } else {
                    lpos.x = 0.;
                }

                self.scroll_terminal(lpos.x, lpos.y, multiplier as f64);
            },
        }
    }

    fn scroll_terminal(&mut self, new_scroll_x_px: f64, new_scroll_y_px: f64, multiplier: f64) {
        const MOUSE_WHEEL_UP: u8 = 64;
        const MOUSE_WHEEL_DOWN: u8 = 65;
        const MOUSE_WHEEL_LEFT: u8 = 66;
        const MOUSE_WHEEL_RIGHT: u8 = 67;

        let width = f64::from(self.ctx.size_info().cell_width());
        let height = f64::from(self.ctx.size_info().cell_height());

        let multiplier = if self.ctx.mouse_mode() { 1. } else { multiplier };

        self.ctx.mouse_mut().accumulated_scroll.x += new_scroll_x_px * multiplier;
        self.ctx.mouse_mut().accumulated_scroll.y += new_scroll_y_px * multiplier;

        let lines = (self.ctx.mouse().accumulated_scroll.y / height).abs() as usize;
        let columns = (self.ctx.mouse().accumulated_scroll.x / width).abs() as usize;

        let is_scroll_up = new_scroll_y_px > 0.;
        let event = if is_scroll_up { MouseEvent::WheelUp } else { MouseEvent::WheelDown };

        if lines != 0 && self.process_mouse_bindings(event) {
            // Repeat for remaining number of lines.
            for _ in 1..lines {
                self.process_mouse_bindings(event);
            }
        } else if self.ctx.mouse_mode() {
            let code = if is_scroll_up { MOUSE_WHEEL_UP } else { MOUSE_WHEEL_DOWN };
            for _ in 0..lines {
                self.mouse_report(code, ElementState::Pressed);
            }

            let code = if new_scroll_x_px > 0. { MOUSE_WHEEL_LEFT } else { MOUSE_WHEEL_RIGHT };
            for _ in 0..columns {
                self.mouse_report(code, ElementState::Pressed);
            }
        } else if self
            .ctx
            .terminal()
            .mode()
            .contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL)
            && !self.ctx.modifiers().state().shift_key()
        {
            // The chars here are the same as for the respective arrow keys.
            let line_cmd = if is_scroll_up { b'A' } else { b'B' };
            let column_cmd = if new_scroll_x_px > 0. { b'D' } else { b'C' };

            let mut content = Vec::with_capacity(3 * (lines + columns));

            for _ in 0..lines {
                content.push(0x1b);
                content.push(b'O');
                content.push(line_cmd);
            }

            for _ in 0..columns {
                content.push(0x1b);
                content.push(b'O');
                content.push(column_cmd);
            }

            self.ctx.write_to_pty(content);
        } else if lines != 0 {
            let lines = if is_scroll_up { lines as i32 } else { -(lines as i32) };
            self.ctx.scroll(Scroll::Delta(lines));
        }

        self.ctx.mouse_mut().accumulated_scroll.x %= width;
        self.ctx.mouse_mut().accumulated_scroll.y %= height;
    }

    pub fn on_focus_change(&mut self, is_focused: bool) {
        if self.ctx.terminal().mode().contains(TermMode::FOCUS_IN_OUT) {
            let chr = if is_focused { "I" } else { "O" };

            let msg = format!("\x1b[{chr}");
            self.ctx.write_to_pty(msg.into_bytes());
        }
    }

    /// Handle touch input.
    pub fn touch(&mut self, touch: TouchEvent) {
        match touch.phase {
            TouchPhase::Started => self.on_touch_start(touch),
            TouchPhase::Moved => self.on_touch_motion(touch),
            TouchPhase::Ended | TouchPhase::Cancelled => self.on_touch_end(touch),
        }
    }

    /// Handle beginning of touch input.
    pub fn on_touch_start(&mut self, touch: TouchEvent) {
        // Inhibit IME on touch while not focused, forcing a touch tap while focused to enable IME.
        if !self.ctx.terminal().is_focused {
            self.ctx.window().set_ime_inhibitor(ImeInhibitor::TOUCH, true);
        }

        let touch_purpose = self.ctx.touch_purpose();
        *touch_purpose = match mem::take(touch_purpose) {
            TouchPurpose::None => TouchPurpose::Tap(touch),
            TouchPurpose::Tap(start) => TouchPurpose::Zoom(TouchZoom::new((start, touch))),
            TouchPurpose::ZoomPendingSlot(slot) => {
                TouchPurpose::Zoom(TouchZoom::new((slot, touch)))
            },
            TouchPurpose::Zoom(zoom) => {
                let slots = zoom.slots();
                let mut set = HashSet::default();
                set.insert(slots.0.id);
                set.insert(slots.1.id);
                TouchPurpose::Invalid(set)
            },
            TouchPurpose::Scroll(event) | TouchPurpose::Select(event) => {
                let mut set = HashSet::default();
                set.insert(event.id);
                TouchPurpose::Invalid(set)
            },
            TouchPurpose::Invalid(mut slots) => {
                slots.insert(touch.id);
                TouchPurpose::Invalid(slots)
            },
        };
    }

    /// Handle touch input movement.
    pub fn on_touch_motion(&mut self, touch: TouchEvent) {
        let touch_purpose = self.ctx.touch_purpose();
        match touch_purpose {
            TouchPurpose::None => (),
            // Handle transition from tap to scroll/select.
            TouchPurpose::Tap(start) => {
                let delta_x = touch.location.x - start.location.x;
                let delta_y = touch.location.y - start.location.y;
                if delta_x.abs() > MAX_TAP_DISTANCE {
                    // Update gesture state.
                    let start_location = start.location;
                    *touch_purpose = TouchPurpose::Select(*start);

                    // Start simulated mouse input.
                    self.mouse_moved(start_location);
                    self.mouse_input(ElementState::Pressed, MouseButton::Left);

                    // Apply motion since touch start.
                    self.on_touch_motion(touch);
                } else if delta_y.abs() > MAX_TAP_DISTANCE {
                    // Update gesture state.
                    *touch_purpose = TouchPurpose::Scroll(*start);

                    // Apply motion since touch start.
                    self.on_touch_motion(touch);
                }
            },
            TouchPurpose::Zoom(zoom) => {
                let font_delta = zoom.font_delta(touch);
                self.ctx.change_font_size(font_delta);
            },
            TouchPurpose::Scroll(last_touch) => {
                // Calculate delta and update last touch position.
                let delta_y = touch.location.y - last_touch.location.y;
                *touch_purpose = TouchPurpose::Scroll(touch);

                // Use a fixed scroll factor for touchscreens, to accurately track finger motion.
                self.scroll_terminal(0., delta_y, 1.0);
            },
            TouchPurpose::Select(_) => self.mouse_moved(touch.location),
            TouchPurpose::ZoomPendingSlot(_) | TouchPurpose::Invalid(_) => (),
        }
    }

    /// Handle end of touch input.
    pub fn on_touch_end(&mut self, touch: TouchEvent) {
        // Finalize the touch motion up to the release point.
        self.on_touch_motion(touch);

        let touch_purpose = self.ctx.touch_purpose();
        match touch_purpose {
            // Simulate LMB clicks.
            TouchPurpose::Tap(start) => {
                let start_location = start.location;
                *touch_purpose = Default::default();

                self.mouse_moved(start_location);
                self.mouse_input(ElementState::Pressed, MouseButton::Left);
                self.mouse_input(ElementState::Released, MouseButton::Left);

                self.ctx.window().set_ime_inhibitor(ImeInhibitor::TOUCH, false);
            },
            // Transition zoom to pending state once a finger was released.
            TouchPurpose::Zoom(zoom) => {
                let slots = zoom.slots();
                let remaining = if slots.0.id == touch.id { slots.1 } else { slots.0 };
                *touch_purpose = TouchPurpose::ZoomPendingSlot(remaining);
            },
            TouchPurpose::ZoomPendingSlot(_) => *touch_purpose = Default::default(),
            // Reset touch state once all slots were released.
            TouchPurpose::Invalid(slots) => {
                slots.remove(&touch.id);
                if slots.is_empty() {
                    *touch_purpose = Default::default();
                }
            },
            // Release simulated LMB.
            TouchPurpose::Select(_) => {
                *touch_purpose = Default::default();
                self.mouse_input(ElementState::Released, MouseButton::Left);
            },
            // Reset touch state on scroll finish.
            TouchPurpose::Scroll(_) => *touch_purpose = Default::default(),
            TouchPurpose::None => (),
        }
    }

    /// Reset mouse cursor based on modifier and terminal state.
    #[inline]
    pub fn reset_mouse_cursor(&mut self) {
        let mouse_state = self.cursor_state();
        self.ctx.window().set_mouse_cursor(mouse_state);
    }

    /// Modifier state change.
    pub fn modifiers_input(&mut self, modifiers: Modifiers) {
        *self.ctx.modifiers() = modifiers;

        // Prompt hint highlight update.
        self.ctx.mouse_mut().hint_highlight_dirty = true;

        // Update mouse state and check for URL change.
        let mouse_state = self.cursor_state();
        self.ctx.window().set_mouse_cursor(mouse_state);
    }

    pub fn mouse_input(&mut self, state: ElementState, button: MouseButton) {
        match button {
            MouseButton::Left => self.ctx.mouse_mut().left_button_state = state,
            MouseButton::Middle => self.ctx.mouse_mut().middle_button_state = state,
            MouseButton::Right => self.ctx.mouse_mut().right_button_state = state,
            _ => (),
        }

        // Skip normal mouse events if the message bar has been clicked.
        self.ctx.mouse_mut().hint_highlight_dirty = true;
        if self.message_bar_cursor_state() == Some(CursorIcon::Pointer)
            && state == ElementState::Pressed
        {
            let size = self.ctx.size_info();

            let current_lines = self.ctx.message().map_or(0, |m| m.text(&size).len());

            self.ctx.clear_selection();
            self.ctx.pop_message();

            // Reset cursor when message bar height changed or all messages are gone.
            let new_lines = self.ctx.message().map_or(0, |m| m.text(&size).len());

            let new_icon = match current_lines.cmp(&new_lines) {
                Ordering::Less => CursorIcon::Default,
                Ordering::Equal => CursorIcon::Pointer,
                Ordering::Greater => self.cursor_state(),
            };

            self.ctx.window().set_mouse_cursor(new_icon);
        } else {
            match state {
                ElementState::Pressed => {
                    // Process mouse press before bindings to update the `click_state`.
                    self.on_mouse_press(button);
                    self.process_mouse_bindings(MouseEvent::Button(button));
                },
                ElementState::Released => self.on_mouse_release(button),
            }
        }
    }

    /// Attempt to find a binding and execute its action.
    ///
    /// The provided mode, mods, and key must match what is allowed by a binding
    /// for its action to be executed.
    fn process_mouse_bindings(&mut self, event: MouseEvent) -> bool {
        let mode = BindingMode::new(self.ctx.terminal().mode(), self.ctx.search_active());
        let mouse_mode = self.ctx.mouse_mode();
        let mods = self.ctx.modifiers().state();
        let mouse_bindings = self.ctx.config().mouse_bindings().to_owned();

        // If mouse mode is active, also look for bindings without shift.
        let fallback_allowed = mouse_mode
            && mods.contains(ModifiersState::SHIFT)
            && !self.ctx.config().mouse.shift_passthrough;
        let mut match_found: bool = false;

        for binding in &mouse_bindings {
            // Don't trigger normal bindings in mouse mode unless Shift is pressed.
            if binding.is_triggered_by(mode, mods, &event) && (fallback_allowed || !mouse_mode) {
                binding.action.execute(&mut self.ctx);
                match_found = true;
            }
        }

        if fallback_allowed && !match_found {
            let fallback_mods = mods & !ModifiersState::SHIFT;
            for binding in &mouse_bindings {
                if binding.is_triggered_by(mode, fallback_mods, &event) {
                    binding.action.execute(&mut self.ctx);
                    match_found = true;
                }
            }
        }

        match_found
    }

    /// Check mouse icon state in relation to the message bar.
    fn message_bar_cursor_state(&self) -> Option<CursorIcon> {
        // Since search is above the message bar, the button is offset by search's height.
        let search_height = usize::from(self.ctx.search_active());

        // Calculate Y position of the end of the last terminal line.
        let size = self.ctx.size_info();
        let terminal_end = size.padding_y() as usize
            + size.cell_height() as usize * (size.screen_lines() + search_height);

        let mouse = self.ctx.mouse();
        let display_offset = self.ctx.terminal().grid().display_offset();
        let point = self.ctx.mouse().point(&self.ctx.size_info(), display_offset);

        if self.ctx.message().is_none() || (mouse.y <= terminal_end) {
            None
        } else if mouse.y <= terminal_end + size.cell_height() as usize
            && point.column + message_bar::CLOSE_BUTTON_TEXT.len() >= size.columns()
        {
            Some(CursorIcon::Pointer)
        } else {
            Some(CursorIcon::Default)
        }
    }

    /// Icon state of the cursor.
    fn cursor_state(&mut self) -> CursorIcon {
        let display_offset = self.ctx.terminal().grid().display_offset();
        let point = self.ctx.mouse().point(&self.ctx.size_info(), display_offset);
        let hyperlink = self.ctx.terminal().grid()[point].hyperlink();

        // Function to check if mouse is on top of a hint.
        let hint_highlighted = |hint: &HintMatch| hint.should_highlight(point, hyperlink.as_ref());

        if let Some(mouse_state) = self.message_bar_cursor_state() {
            mouse_state
        } else {
            let highlighted = self.ctx.selection_is_empty()
                && self.ctx.display().highlighted_hint.as_ref().is_some_and(hint_highlighted);
            mouse_cursor_icon(
                self.ctx.modifiers().state(),
                self.ctx.mouse_mode(),
                self.ctx.config().mouse.shift_passthrough,
                highlighted,
                self.ctx.mouse().is_left_drag(),
            )
        }
    }

    /// Handle automatic scrolling when selecting above/below the window.
    fn update_selection_scrolling(&mut self, mouse_y: i32) {
        let scale_factor = self.ctx.window().scale_factor;
        let size = self.ctx.size_info();
        let window_id = self.ctx.window().id();
        let scheduler = self.ctx.scheduler_mut();

        // Scale constants by DPI.
        let step = ((SELECTION_SCROLLING_STEP * scale_factor) as i32).max(1);
        // Padding belongs to the window: scrolling starts at its edge, not in the first text row.
        let end_top = SELECTION_SCROLLING_EDGE;
        let start_bottom = size.height() as i32 - SELECTION_SCROLLING_EDGE;

        // Get distance from closest window boundary.
        let delta = if mouse_y < end_top {
            end_top - mouse_y + step
        } else if mouse_y >= start_bottom {
            start_bottom - mouse_y - step
        } else {
            scheduler.unschedule(TimerId::new(Topic::SelectionScrolling, window_id));
            return;
        };

        // Scale number of lines scrolled based on distance to boundary.
        let event = Event::new(EventType::SelectionScroll(delta / step), Some(window_id));

        // Schedule event.
        let timer_id = TimerId::new(Topic::SelectionScrolling, window_id);
        scheduler.schedule_or_update(event, SELECTION_SCROLLING_INTERVAL, true, timer_id);
    }
}

#[cfg(test)]
mod tests {
    use super::ActionContext as _;
    use super::*;

    use winit::event::{DeviceId, Event as WinitEvent, WindowEvent};
    use winit::keyboard::Key;
    use winit::window::WindowId;

    use alacritty_terminal::event::Event as TerminalEvent;

    use crate::config::Binding;
    use crate::message_bar::MessageBuffer;

    const KEY: Key<&'static str> = Key::Character("0");

    struct MockEventProxy;
    impl EventListener for MockEventProxy {}

    struct ActionContext<'a, T> {
        pub terminal: &'a mut Term<T>,
        pub size_info: &'a SizeInfo,
        pub mouse: &'a mut Mouse,
        pub clipboard: &'a mut Clipboard,
        pub message_buffer: &'a mut MessageBuffer,
        pub modifiers: Modifiers,
        config: &'a UiConfig,
        inline_search_state: &'a mut InlineSearchState,
        pty_output: std::cell::RefCell<Vec<u8>>,
    }

    impl<T: EventListener> super::ActionContext<T> for ActionContext<'_, T> {
        fn write_to_pty<B: Into<Cow<'static, [u8]>>>(&self, data: B) {
            self.pty_output.borrow_mut().extend_from_slice(&data.into());
        }

        fn search_next(
            &mut self,
            _origin: Point,
            _direction: Direction,
            _side: Side,
        ) -> Option<Match> {
            None
        }

        fn search_direction(&self) -> Direction {
            Direction::Right
        }

        fn inline_search_state(&mut self) -> &mut InlineSearchState {
            self.inline_search_state
        }

        fn search_active(&self) -> bool {
            false
        }

        fn terminal(&self) -> &Term<T> {
            self.terminal
        }

        fn terminal_mut(&mut self) -> &mut Term<T> {
            self.terminal
        }

        fn size_info(&self) -> SizeInfo {
            *self.size_info
        }

        fn selection_is_empty(&self) -> bool {
            self.terminal
                .selection
                .as_ref()
                .is_none_or(alacritty_terminal::selection::Selection::is_empty)
        }

        fn clear_selection(&mut self) {
            self.terminal.selection = None;
        }

        fn start_selection(&mut self, ty: SelectionType, point: Point, side: Side) {
            self.terminal.selection =
                Some(alacritty_terminal::selection::Selection::new(ty, point, side));
        }

        fn update_selection(&mut self, point: Point, side: Side) {
            if let Some(selection) = &mut self.terminal.selection {
                selection.update(point, side);
            }
        }

        fn scroll(&mut self, scroll: Scroll) {
            self.terminal.scroll_display(scroll);
        }

        fn mouse_mode(&self) -> bool {
            self.terminal.mode().intersects(TermMode::MOUSE_MODE)
        }

        #[inline]
        fn mouse_mut(&mut self) -> &mut Mouse {
            self.mouse
        }

        #[inline]
        fn mouse(&self) -> &Mouse {
            self.mouse
        }

        #[inline]
        fn touch_purpose(&mut self) -> &mut TouchPurpose {
            unimplemented!();
        }

        fn modifiers(&mut self) -> &mut Modifiers {
            &mut self.modifiers
        }

        fn window(&mut self) -> &mut Window {
            unimplemented!();
        }

        fn display(&mut self) -> &mut Display {
            unimplemented!();
        }

        fn pop_message(&mut self) {
            self.message_buffer.pop();
        }

        fn message(&self) -> Option<&Message> {
            self.message_buffer.message()
        }

        fn config(&self) -> &UiConfig {
            self.config
        }

        fn clipboard_mut(&mut self) -> &mut Clipboard {
            self.clipboard
        }

        #[cfg(target_os = "macos")]
        fn event_loop(&self) -> &ActiveEventLoop {
            unimplemented!();
        }

        fn scheduler_mut(&mut self) -> &mut Scheduler {
            unimplemented!();
        }

        fn semantic_word(&self, _point: Point) -> String {
            unimplemented!();
        }
    }

    #[test]
    fn pixel_scroll_preserves_motion_at_gesture_and_momentum_boundaries() {
        // The same distances must reach both terminal history and a mouse-aware
        // application such as tmux, regardless of the phase carrying the delta.
        for reporting in [false, true] {
            let mut cfg = UiConfig::default();
            cfg.scrolling.multiplier = 1;
            let size = SizeInfo::new(120., 60., 6., 12., 0., 0., false);
            let mut terminal = Term::new(cfg.term_options(), &size, MockEventProxy);
            let mut parser: alacritty_terminal::vte::ansi::Processor = Default::default();
            parser.advance(&mut terminal, "history line\r\n".repeat(100).as_bytes());
            if reporting {
                parser.advance(&mut terminal, b"\x1b[?1000h\x1b[?1006h");
            }
            let selection = alacritty_terminal::selection::Selection::new(
                SelectionType::Simple,
                Point::new(alacritty_terminal::index::Line(0), Column(0)),
                Side::Left,
            );
            terminal.selection = Some(selection.clone());
            let mut mouse = Mouse::default();
            let mut clipboard = Clipboard::new_nop();
            let mut message_buffer = MessageBuffer::default();
            let mut inline_search_state = InlineSearchState::default();
            let mut processor = Processor::new(ActionContext {
                terminal: &mut terminal,
                size_info: &size,
                mouse: &mut mouse,
                clipboard: &mut clipboard,
                message_buffer: &mut message_buffer,
                modifiers: Default::default(),
                config: &cfg,
                inline_search_state: &mut inline_search_state,
                pty_output: Default::default(),
            });

            for (phase, pixels, total) in [
                (TouchPhase::Started, 6., 0),
                (TouchPhase::Moved, 6., 1),
                (TouchPhase::Ended, 12., 2),
                (TouchPhase::Started, 6., 2),
                (TouchPhase::Ended, 0., 2),
                // macOS sends a new Started event for momentum.
                (TouchPhase::Started, 6., 3),
                (TouchPhase::Moved, 12., 4),
                (TouchPhase::Ended, 0., 4),
                (TouchPhase::Cancelled, 0., 4),
            ] {
                processor.mouse_wheel_input(
                    MouseScrollDelta::PixelDelta(PhysicalPosition::new(0., pixels)),
                    phase,
                );
                if reporting {
                    assert_eq!(
                        *processor.ctx.pty_output.borrow(),
                        b"\x1b[<64;1;1M".repeat(total),
                        "phase {phase:?} lost or duplicated motion",
                    );
                } else {
                    assert_eq!(processor.ctx.terminal.grid().display_offset(), total);
                    assert_eq!(processor.ctx.terminal.selection.as_ref(), Some(&selection));
                }
            }
        }
    }

    #[test]
    fn shift_passthrough_keeps_mouse_events_in_the_application() {
        assert!(mouse_reporting_override(ModifiersState::SHIFT, false));
        assert!(!mouse_reporting_override(ModifiersState::SHIFT, true));
        assert!(!mouse_reporting_override(ModifiersState::empty(), true));
        #[cfg(target_os = "macos")]
        {
            assert!(mouse_reporting_override(ModifiersState::SUPER, true));
            assert!(mouse_reporting_override(ModifiersState::SUPER | ModifiersState::SHIFT, true));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn command_click_bypasses_application_mouse_reporting() {
        for (modifiers, shift_passthrough) in [
            (ModifiersState::empty(), false),
            (ModifiersState::SUPER, false),
            (ModifiersState::SHIFT, false),
            (ModifiersState::CONTROL, false),
            (ModifiersState::ALT, false),
            (ModifiersState::SHIFT, true),
            (ModifiersState::SUPER, true),
        ] {
            let mut cfg = UiConfig::default();
            cfg.mouse.shift_passthrough = shift_passthrough;
            let size = SizeInfo::new(120., 60., 6., 12., 0., 0., false);
            let mut terminal = Term::new(cfg.term_options(), &size, MockEventProxy);
            let mut parser: alacritty_terminal::vte::ansi::Processor = Default::default();
            parser.advance(&mut terminal, b"\x1b[?1000h");
            let mut mouse = Mouse { block_hint_launcher: true, ..Mouse::default() };
            let mut clipboard = Clipboard::new_nop();
            let mut message_buffer = MessageBuffer::default();
            let mut inline_search_state = InlineSearchState::default();
            let context = ActionContext {
                terminal: &mut terminal,
                mouse: &mut mouse,
                size_info: &size,
                clipboard: &mut clipboard,
                modifiers: modifiers.into(),
                message_buffer: &mut message_buffer,
                inline_search_state: &mut inline_search_state,
                pty_output: Default::default(),
                config: &cfg,
            };
            let mut processor = Processor::new(context);
            processor.on_mouse_press(MouseButton::Left);
            if modifiers == ModifiersState::SUPER
                || (modifiers == ModifiersState::SHIFT && !shift_passthrough)
            {
                assert_eq!(processor.ctx.mouse.click_state, ClickState::Click);
                assert!(!processor.ctx.mouse.block_hint_launcher);
            } else {
                assert_eq!(processor.ctx.mouse.click_state, ClickState::None);
                assert!(processor.ctx.mouse.block_hint_launcher);
                assert!(processor.ctx.terminal.selection.is_none());
                // A forwarded release must not fall through to native selection/hint handling.
                processor.on_mouse_release(MouseButton::Left);
            }
        }
    }

    #[test]
    fn shift_click_extends_selection_across_scrollback_without_resetting_anchor() {
        use alacritty_terminal::index::Line;
        use alacritty_terminal::selection::Selection;

        for mouse_reporting in [false, true] {
            for ty in [
                SelectionType::Simple,
                SelectionType::Block,
                SelectionType::Semantic,
                SelectionType::Lines,
            ] {
                let cfg = UiConfig::default();
                let size = SizeInfo::new(120., 60., 6., 12., 0., 0., false);
                let mut terminal = Term::new(cfg.term_options(), &size, MockEventProxy);
                let mut parser: alacritty_terminal::vte::ansi::Processor = Default::default();
                parser.advance(&mut terminal, "history line\r\n".repeat(100).as_bytes());
                if mouse_reporting {
                    parser.advance(&mut terminal, b"\x1b[?1000h\x1b[?1006h");
                }
                terminal.scroll_display(Scroll::Delta(80));
                let anchor = Point::new(Line(-80), Column(1));
                terminal.selection = Some(Selection::new(ty, anchor, Side::Left));
                let mut mouse = Mouse {
                    x: 22,
                    y: 37,
                    cell_side: Side::Right,
                    click_state: ClickState::DoubleClick,
                    last_click_button: MouseButton::Left,
                    ..Mouse::default()
                };
                let mut clipboard = Clipboard::new_nop();
                let mut message_buffer = MessageBuffer::default();
                let mut inline_search_state = InlineSearchState::default();
                let mut processor = Processor::new(ActionContext {
                    terminal: &mut terminal,
                    size_info: &size,
                    mouse: &mut mouse,
                    clipboard: &mut clipboard,
                    message_buffer: &mut message_buffer,
                    modifiers: ModifiersState::SHIFT.into(),
                    config: &cfg,
                    inline_search_state: &mut inline_search_state,
                    pty_output: Default::default(),
                });

                // The first click can be only an empty anchor, or an existing dragged range.
                for dragged in [false, true] {
                    let mut expected = Selection::new(ty, anchor, Side::Left);
                    if dragged {
                        expected.update(Point::new(Line(-79), Column(4)), Side::Right);
                    }
                    processor.ctx.terminal.selection = Some(expected.clone());
                    processor.ctx.terminal.scroll_display(Scroll::Delta(-50));
                    let endpoint = processor
                        .ctx
                        .mouse
                        .point(&size, processor.ctx.terminal.grid().display_offset());
                    expected.update(endpoint, Side::Right);
                    processor.on_mouse_press(MouseButton::Left);
                    assert_eq!(processor.ctx.terminal.selection.as_ref(), Some(&expected));
                    assert!(processor.ctx.mouse.block_hint_launcher);
                    assert_eq!(processor.ctx.mouse.click_state, ClickState::Click);

                    // A later Shift-click can cross the original anchor in the other direction.
                    processor.ctx.terminal.scroll_display(Scroll::Top);
                    let endpoint = processor
                        .ctx
                        .mouse
                        .point(&size, processor.ctx.terminal.grid().display_offset());
                    expected.update(endpoint, Side::Right);
                    processor.on_mouse_press(MouseButton::Left);
                    assert_eq!(processor.ctx.terminal.selection.as_ref(), Some(&expected));
                    processor.ctx.terminal.scroll_display(Scroll::Bottom);
                    processor.ctx.terminal.scroll_display(Scroll::Delta(80));
                }
            }
        }
    }

    #[test]
    fn shift_click_without_anchor_and_plain_click_start_new_selections() {
        use alacritty_terminal::index::Line;
        use alacritty_terminal::selection::Selection;
        let cfg = UiConfig::default();
        let size = SizeInfo::new(120., 60., 6., 12., 0., 0., false);
        let mut terminal = Term::new(cfg.term_options(), &size, MockEventProxy);
        let mut mouse =
            Mouse { cell_side: Side::Right, click_state: ClickState::Click, ..Mouse::default() };
        let mut clipboard = Clipboard::new_nop();
        let mut message_buffer = MessageBuffer::default();
        let mut inline_search_state = InlineSearchState::default();
        let mut processor = Processor::new(ActionContext {
            terminal: &mut terminal,
            size_info: &size,
            mouse: &mut mouse,
            clipboard: &mut clipboard,
            message_buffer: &mut message_buffer,
            modifiers: ModifiersState::SHIFT.into(),
            config: &cfg,
            inline_search_state: &mut inline_search_state,
            pty_output: Default::default(),
        });
        let first = Point::new(Line(0), Column(1));
        processor.on_left_click(first);
        assert_eq!(
            processor.ctx.terminal.selection,
            Some(Selection::new(SelectionType::Simple, first, Side::Right))
        );
        processor.ctx.modifiers = ModifiersState::empty().into();
        let second = Point::new(Line(3), Column(2));
        processor.on_left_click(second);
        assert_eq!(
            processor.ctx.terminal.selection,
            Some(Selection::new(SelectionType::Simple, second, Side::Right))
        );
    }

    #[test]
    fn repeated_clicks_require_nearby_positions_and_no_previous_drag() {
        let cfg = UiConfig::default();
        let size = SizeInfo::new(200., 100., 10., 20., 0., 0., false);
        let mut terminal = Term::new(cfg.term_options(), &size, MockEventProxy);
        let mut mouse = Mouse::default();
        let mut clipboard = Clipboard::new_nop();
        let mut messages = MessageBuffer::default();
        let mut inline_search = InlineSearchState::default();
        let mut processor = Processor::new(ActionContext {
            terminal: &mut terminal,
            size_info: &size,
            mouse: &mut mouse,
            clipboard: &mut clipboard,
            message_buffer: &mut messages,
            modifiers: Default::default(),
            config: &cfg,
            inline_search_state: &mut inline_search,
            pty_output: Default::default(),
        });

        for (x, y, dragged, expected) in [
            (20, 20, false, ClickState::Click),
            (26, 28, false, ClickState::DoubleClick), // One cell-width Euclidean distance.
            (29, 29, false, ClickState::Click),       // Still measured from the first press.
            (29, 29, false, ClickState::DoubleClick),
            (29, 29, false, ClickState::TripleClick),
            (29, 29, false, ClickState::TripleClick),
            (29, 29, true, ClickState::Click), // Dragged away and returned to the press.
            (29, 50, false, ClickState::Click), // A different row is a new selection.
        ] {
            processor.ctx.mouse.x = x;
            processor.ctx.mouse.y = y;
            processor.ctx.mouse.click_dragged = dragged;
            processor.on_mouse_press(MouseButton::Left);
            assert_eq!(processor.ctx.mouse.click_state, expected);
        }

        // The macOS Option gesture must start a rectangle, even immediately after a click.
        #[cfg(target_os = "macos")]
        {
            processor.ctx.modifiers = ModifiersState::ALT.into();
            for _ in 0..3 {
                processor.on_mouse_press(MouseButton::Left);
                assert_eq!(
                    processor.ctx.terminal.selection.as_ref().unwrap().ty,
                    SelectionType::Block
                );
                assert!(processor.ctx.mouse.block_hint_launcher);
            }
        }
    }

    #[test]
    fn selection_threshold_tracks_sixty_percent_and_clamps_padding() {
        for scale in [1., 2.] {
            let cfg = UiConfig::default();
            let size = SizeInfo::new(
                220. * scale,
                120. * scale,
                10. * scale,
                20. * scale,
                10. * scale,
                20. * scale,
                false,
            );
            let mut terminal = Term::new(cfg.term_options(), &size, MockEventProxy);
            let mut parser: alacritty_terminal::vte::ansi::Processor = Default::default();
            parser.advance(&mut terminal, b"ABCDE");
            let mut mouse = Mouse { y: (20. * scale) as usize, ..Mouse::default() };
            let mut clipboard = Clipboard::new_nop();
            let mut messages = MessageBuffer::default();
            let mut inline_search = InlineSearchState::default();
            let mut processor = Processor::new(ActionContext {
                terminal: &mut terminal,
                size_info: &size,
                mouse: &mut mouse,
                clipboard: &mut clipboard,
                message_buffer: &mut messages,
                modifiers: Default::default(),
                config: &cfg,
                inline_search_state: &mut inline_search,
                pty_output: Default::default(),
            });

            assert_eq!(processor.cell_side((9. * scale) as usize), Side::Left);
            assert_eq!(processor.cell_side((211. * scale) as usize), Side::Right);
            for (start, end, expected) in
                [(12., 15., None), (12., 16., Some("A")), (18., 16., None), (18., 15., Some("A"))]
            {
                processor.ctx.mouse.x = (start * scale) as usize;
                processor.ctx.mouse.cell_side = processor.cell_side(processor.ctx.mouse.x);
                processor.ctx.mouse.click_state = ClickState::Click;
                let point = processor.ctx.mouse.point(&size, 0);
                processor.on_left_click(point);
                processor.ctx.mouse.x = (end * scale) as usize;
                let point = processor.ctx.mouse.point(&size, 0);
                processor.ctx.update_selection(point, processor.cell_side(processor.ctx.mouse.x));
                assert_eq!(processor.ctx.terminal.selection_to_string().as_deref(), expected);
            }
        }
    }

    #[test]
    fn selection_pointer_matches_modifiers_and_application_capture() {
        assert_eq!(
            mouse_cursor_icon(ModifiersState::empty(), false, false, false, false),
            CursorIcon::Text
        );
        assert_eq!(
            mouse_cursor_icon(ModifiersState::empty(), false, false, true, false),
            CursorIcon::Pointer
        );
        assert_eq!(
            mouse_cursor_icon(ModifiersState::CONTROL, false, false, true, false),
            CursorIcon::Crosshair
        );
        // Mouse-aware UI (including tmux's tab bar) owns ordinary clicks.
        assert_eq!(
            mouse_cursor_icon(ModifiersState::empty(), true, false, false, false),
            CursorIcon::Default
        );
        assert_eq!(
            mouse_cursor_icon(ModifiersState::empty(), true, false, true, false),
            CursorIcon::Default
        );
        assert_eq!(
            mouse_cursor_icon(ModifiersState::empty(), true, false, true, true),
            CursorIcon::Text
        );
        for passthrough in [false, true] {
            assert_eq!(
                mouse_cursor_icon(ModifiersState::SHIFT, true, passthrough, false, false),
                CursorIcon::Text
            );
            assert_eq!(
                mouse_cursor_icon(ModifiersState::SHIFT, false, passthrough, true, false),
                CursorIcon::Text
            );
        }
        let mut mouse = Mouse {
            left_button_state: ElementState::Pressed,
            click_dragged: true,
            ..Mouse::default()
        };
        assert!(mouse.is_left_drag());
        mouse.left_button_state = ElementState::Released;
        assert!(!mouse.is_left_drag());
        #[cfg(target_os = "macos")]
        {
            assert_eq!(
                mouse_cursor_icon(ModifiersState::SUPER, true, false, false, false),
                CursorIcon::Text
            );
            assert_eq!(
                mouse_cursor_icon(ModifiersState::ALT, false, false, true, false),
                CursorIcon::Crosshair
            );
            assert_eq!(
                mouse_cursor_icon(ModifiersState::ALT, true, false, false, false),
                CursorIcon::Default
            );
            assert_eq!(
                mouse_cursor_icon(
                    ModifiersState::SHIFT | ModifiersState::ALT,
                    true,
                    false,
                    false,
                    false
                ),
                CursorIcon::Crosshair
            );
            assert_eq!(
                mouse_cursor_icon(
                    ModifiersState::SHIFT | ModifiersState::ALT,
                    true,
                    true,
                    false,
                    false
                ),
                CursorIcon::Text
            );
            assert_eq!(
                mouse_cursor_icon(
                    ModifiersState::SUPER | ModifiersState::ALT,
                    true,
                    true,
                    false,
                    false
                ),
                CursorIcon::Crosshair
            );
        }
    }

    #[test]
    fn queued_selection_scroll_requires_a_live_native_gesture() {
        let mut cfg = UiConfig::default();
        cfg.mouse.shift_passthrough = true;
        let size = SizeInfo::new(120., 60., 6., 12., 0., 0., false);
        let mut terminal = Term::new(cfg.term_options(), &size, MockEventProxy);
        let mut parser: alacritty_terminal::vte::ansi::Processor = Default::default();
        parser.advance(&mut terminal, "history\r\n".repeat(100).as_bytes());
        let mut mouse = Mouse::default();
        let mut clipboard = Clipboard::new_nop();
        let mut messages = MessageBuffer::default();
        let mut inline_search = InlineSearchState::default();
        let mut processor = Processor::new(ActionContext {
            terminal: &mut terminal,
            size_info: &size,
            mouse: &mut mouse,
            clipboard: &mut clipboard,
            message_buffer: &mut messages,
            modifiers: Default::default(),
            config: &cfg,
            inline_search_state: &mut inline_search,
            pty_output: Default::default(),
        });

        for (pressed, owned, reporting, modifiers, selected, allowed) in [
            (true, true, false, ModifiersState::empty(), true, true),
            (false, true, false, ModifiersState::empty(), true, false),
            (true, false, false, ModifiersState::empty(), true, false),
            (true, true, false, ModifiersState::empty(), false, false),
            (true, true, true, ModifiersState::empty(), true, false),
            (true, true, true, ModifiersState::SHIFT, true, false),
            (true, false, true, ModifiersState::SUPER, true, false),
        ] {
            processor.ctx.terminal.scroll_display(Scroll::Bottom);
            processor.ctx.terminal.selection = None;
            if selected {
                use alacritty_terminal::index::Line;
                processor.ctx.start_selection(
                    SelectionType::Simple,
                    Point::new(Line(0), Column(0)),
                    Side::Left,
                );
                processor.ctx.update_selection(Point::new(Line(0), Column(2)), Side::Right);
            }
            parser.advance(
                processor.ctx.terminal,
                if reporting { b"\x1b[?1000h" } else { b"\x1b[?1000l" },
            );
            processor.ctx.mouse.left_button_state =
                if pressed { ElementState::Pressed } else { ElementState::Released };
            processor.ctx.mouse.click_state =
                if owned { ClickState::Click } else { ClickState::None };
            processor.ctx.modifiers = modifiers.into();
            assert_eq!(processor.scroll_selection(1), allowed);
            assert_eq!(processor.ctx.terminal.grid().display_offset(), usize::from(allowed));
        }

        #[cfg(target_os = "macos")]
        {
            processor.ctx.mouse.click_state = ClickState::Click;
            assert!(processor.scroll_selection(1), "Command can override application capture");
        }
    }

    macro_rules! test_clickstate {
        {
            name: $name:ident,
            initial_state: $initial_state:expr,
            initial_button: $initial_button:expr,
            input: $input:expr,
            end_state: $end_state:expr,
            input_delay: $input_delay:expr,
        } => {
            #[test]
            fn $name() {
                let mut clipboard = Clipboard::new_nop();
                let cfg = UiConfig::default();
                let size = SizeInfo::new(
                    21.0,
                    51.0,
                    3.0,
                    3.0,
                    0.,
                    0.,
                    false,
                );

                let mut terminal = Term::new(cfg.term_options(), &size, MockEventProxy);

                let mut mouse = Mouse {
                    click_state: $initial_state,
                    last_click_button: $initial_button,
                    last_click_timestamp: Instant::now() - $input_delay,
                    ..Mouse::default()
                };

                let mut inline_search_state = InlineSearchState::default();
                let mut message_buffer = MessageBuffer::default();

                let context = ActionContext {
                    terminal: &mut terminal,
                    mouse: &mut mouse,
                    size_info: &size,
                    clipboard: &mut clipboard,
                    modifiers: Default::default(),
                    message_buffer: &mut message_buffer,
                    inline_search_state: &mut inline_search_state,
                    pty_output: Default::default(),
                    config: &cfg,
                };

                let mut processor = Processor::new(context);

                let event: WinitEvent::<TerminalEvent> = $input;
                if let WinitEvent::WindowEvent {
                    event: WindowEvent::MouseInput {
                        state,
                        button,
                        ..
                    },
                    ..
                } = event
                {
                    processor.mouse_input(state, button);
                };

                assert_eq!(processor.ctx.mouse.click_state, $end_state);
            }
        }
    }

    macro_rules! test_process_binding {
        {
            name: $name:ident,
            binding: $binding:expr,
            triggers: $triggers:expr,
            mode: $mode:expr,
            mods: $mods:expr,
        } => {
            #[test]
            fn $name() {
                if $triggers {
                    assert!($binding.is_triggered_by($mode, $mods, &KEY));
                } else {
                    assert!(!$binding.is_triggered_by($mode, $mods, &KEY));
                }
            }
        }
    }

    test_clickstate! {
        name: single_click,
        initial_state: ClickState::None,
        initial_button: MouseButton::Other(0),
        input: WinitEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                device_id: DeviceId::dummy(),
            },
            window_id: WindowId::dummy(),
        },
        end_state: ClickState::Click,
        input_delay: Duration::ZERO,
    }

    test_clickstate! {
        name: single_right_click,
        initial_state: ClickState::None,
        initial_button: MouseButton::Other(0),
        input: WinitEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                device_id: DeviceId::dummy(),
            },
            window_id: WindowId::dummy(),
        },
        end_state: ClickState::Click,
        input_delay: Duration::ZERO,
    }

    test_clickstate! {
        name: single_middle_click,
        initial_state: ClickState::None,
        initial_button: MouseButton::Other(0),
        input: WinitEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Middle,
                device_id: DeviceId::dummy(),
            },
            window_id: WindowId::dummy(),
        },
        end_state: ClickState::Click,
        input_delay: Duration::ZERO,
    }

    test_clickstate! {
        name: double_click,
        initial_state: ClickState::Click,
        initial_button: MouseButton::Left,
        input: WinitEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                device_id: DeviceId::dummy(),
            },
            window_id: WindowId::dummy(),
        },
        end_state: ClickState::DoubleClick,
        input_delay: Duration::ZERO,
    }

    test_clickstate! {
        name: double_click_failed,
        initial_state: ClickState::Click,
        initial_button: MouseButton::Left,
        input: WinitEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                device_id: DeviceId::dummy(),
            },
            window_id: WindowId::dummy(),
        },
        end_state: ClickState::Click,
        input_delay: click_threshold(),
    }

    test_clickstate! {
        name: triple_click,
        initial_state: ClickState::DoubleClick,
        initial_button: MouseButton::Left,
        input: WinitEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                device_id:  DeviceId::dummy(),
            },
            window_id:  WindowId::dummy(),
        },
        end_state: ClickState::TripleClick,
        input_delay: Duration::ZERO,
    }

    test_clickstate! {
        name: triple_click_failed,
        initial_state: ClickState::DoubleClick,
        initial_button: MouseButton::Left,
        input: WinitEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                device_id: DeviceId::dummy(),
            },
            window_id: WindowId::dummy(),
        },
        end_state: ClickState::Click,
        input_delay: click_threshold(),
    }

    test_clickstate! {
        name: multi_click_separate_buttons,
        initial_state: ClickState::DoubleClick,
        initial_button: MouseButton::Left,
        input: WinitEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                device_id: DeviceId::dummy(),
            },
            window_id: WindowId::dummy(),
        },
        end_state: ClickState::Click,
        input_delay: Duration::ZERO,
    }

    test_process_binding! {
        name: process_binding_nomode_shiftmod_require_shift,
        binding: Binding { trigger: KEY, mods: ModifiersState::SHIFT, action: Action::from("\x1b[1;2D"), mode: BindingMode::empty(), notmode: BindingMode::empty() },
        triggers: true,
        mode: BindingMode::empty(),
        mods: ModifiersState::SHIFT,
    }

    test_process_binding! {
        name: process_binding_nomode_nomod_require_shift,
        binding: Binding { trigger: KEY, mods: ModifiersState::SHIFT, action: Action::from("\x1b[1;2D"), mode: BindingMode::empty(), notmode: BindingMode::empty() },
        triggers: false,
        mode: BindingMode::empty(),
        mods: ModifiersState::empty(),
    }

    test_process_binding! {
        name: process_binding_nomode_controlmod,
        binding: Binding { trigger: KEY, mods: ModifiersState::CONTROL, action: Action::from("\x1b[1;5D"), mode: BindingMode::empty(), notmode: BindingMode::empty() },
        triggers: true,
        mode: BindingMode::empty(),
        mods: ModifiersState::CONTROL,
    }

    test_process_binding! {
        name: process_binding_nomode_nomod_require_not_appcursor,
        binding: Binding { trigger: KEY, mods: ModifiersState::empty(), action: Action::from("\x1b[D"), mode: BindingMode::empty(), notmode: BindingMode::APP_CURSOR },
        triggers: true,
        mode: BindingMode::empty(),
        mods: ModifiersState::empty(),
    }

    test_process_binding! {
        name: process_binding_appcursormode_nomod_require_appcursor,
        binding: Binding { trigger: KEY, mods: ModifiersState::empty(), action: Action::from("\x1bOD"), mode: BindingMode::APP_CURSOR, notmode: BindingMode::empty() },
        triggers: true,
        mode: BindingMode::APP_CURSOR,
        mods: ModifiersState::empty(),
    }

    test_process_binding! {
        name: process_binding_nomode_nomod_require_appcursor,
        binding: Binding { trigger: KEY, mods: ModifiersState::empty(), action: Action::from("\x1bOD"), mode: BindingMode::APP_CURSOR, notmode: BindingMode::empty() },
        triggers: false,
        mode: BindingMode::empty(),
        mods: ModifiersState::empty(),
    }

    test_process_binding! {
        name: process_binding_appcursormode_appkeypadmode_nomod_require_appcursor,
        binding: Binding { trigger: KEY, mods: ModifiersState::empty(), action: Action::from("\x1bOD"), mode: BindingMode::APP_CURSOR, notmode: BindingMode::empty() },
        triggers: true,
        mode: BindingMode::APP_CURSOR | BindingMode::APP_KEYPAD,
        mods: ModifiersState::empty(),
    }

    test_process_binding! {
        name: process_binding_fail_with_extra_mods,
        binding: Binding { trigger: KEY, mods: ModifiersState::SUPER, action: Action::from("arst"), mode: BindingMode::empty(), notmode: BindingMode::empty() },
        triggers: false,
        mode: BindingMode::empty(),
        mods: ModifiersState::ALT | ModifiersState::SUPER,
    }
}
