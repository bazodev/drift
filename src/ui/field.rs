//! A single-line text field.

use gpui::{
    App, Context, CursorStyle, EventEmitter, FocusHandle, Focusable, KeyDownEvent, SharedString, Window, div,
    prelude::*, px, rgba,
};

use super::{Dismiss, FAINT, NextTrack, Paste, PreviousTrack, TEXT, TogglePlay, VolumeDown, VolumeUp};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FieldEvent {
    Changed,
    Submit,
    Cancel,
    Up,
    Down,
}

pub struct TextField {
    pub text: String,
    cursor: usize,
    focus: FocusHandle,
    placeholder: SharedString,
}

impl EventEmitter<FieldEvent> for TextField {}

impl Focusable for TextField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TextField {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self { text: String::new(), cursor: 0, focus: cx.focus_handle(), placeholder: placeholder.into() }
    }

    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.text = text.into();
        self.cursor = self.text.len();
        cx.notify();
    }

    fn previous(&self) -> usize {
        self.text[..self.cursor].char_indices().next_back().map_or(0, |(index, _)| index)
    }

    fn following(&self) -> usize {
        self.text[self.cursor..].chars().next().map_or(self.cursor, |ch| self.cursor + ch.len_utf8())
    }

    fn previous_word(&self) -> usize {
        let before = self.text[..self.cursor].trim_end();
        before.rfind(char::is_whitespace).map_or(0, |index| index + 1)
    }

    fn insert(&mut self, value: &str, cx: &mut Context<Self>) {
        let clean: String = value.chars().filter(|ch| !ch.is_control()).collect();
        if clean.is_empty() {
            return;
        }
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
        cx.emit(FieldEvent::Changed);
        cx.notify();
    }

    fn delete_to(&mut self, start: usize, end: usize, cx: &mut Context<Self>) {
        if start == end {
            return;
        }
        self.text.replace_range(start..end, "");
        self.cursor = start;
        cx.emit(FieldEvent::Changed);
        cx.notify();
    }

    fn move_to(&mut self, position: usize, cx: &mut Context<Self>) {
        self.cursor = position;
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        let (command, option) = (key.modifiers.platform, key.modifiers.alt);
        match key.key.as_str() {
            "enter" => cx.emit(FieldEvent::Submit),
            "up" => cx.emit(FieldEvent::Up),
            "down" => cx.emit(FieldEvent::Down),
            "backspace" => {
                let start = if command {
                    0
                } else if option {
                    self.previous_word()
                } else {
                    self.previous()
                };
                self.delete_to(start, self.cursor, cx);
            }
            "delete" => {
                let end = self.following();
                self.delete_to(self.cursor, end, cx);
            }
            "left" => {
                let target = if option { self.previous_word() } else { self.previous() };
                self.move_to(target, cx);
            }
            "right" => {
                let target = self.following();
                self.move_to(target, cx);
            }
            "home" => self.move_to(0, cx),
            "end" => self.move_to(self.text.len(), cx),
            _ if command || key.modifiers.control => return,
            _ => {
                if let Some(value) = key.key_char.clone() {
                    self.insert(&value, cx);
                }
                return;
            }
        }
        cx.stop_propagation();
    }
}

impl Render for TextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let caret_color = super::accent(cx);
        let caret = move || focused.then(|| div().w(px(1.5)).h(px(17.)).flex_none().bg(rgba(caret_color)));
        let (before, after) = self.text.split_at(self.cursor);
        let (before, after) = (before.to_string(), after.to_string());
        let empty = self.text.is_empty();
        div()
            .key_context("TextField")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_action(cx.listener(|field, _: &TogglePlay, _, cx| field.insert(" ", cx)))
            .on_action(cx.listener(|field, _: &PreviousTrack, _, cx| field.move_to(0, cx)))
            .on_action(cx.listener(|field, _: &NextTrack, _, cx| {
                let end = field.text.len();
                field.move_to(end, cx);
            }))
            .on_action(|_: &VolumeUp, _, _| {})
            .on_action(|_: &VolumeDown, _, _| {})
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| {
                cx.emit(FieldEvent::Cancel);
                cx.propagate();
            }))
            .on_action(cx.listener(|field, _: &Paste, _, cx| {
                if let Some(value) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    field.insert(&value, cx);
                }
            }))
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_sm()
            .text_color(rgba(TEXT))
            .cursor(CursorStyle::IBeam)
            .when(empty, |field| {
                field.children(caret()).child(div().text_color(rgba(FAINT)).child(self.placeholder.clone()))
            })
            .when(!empty, |field| {
                field
                    .when(!before.is_empty(), |field| field.child(before))
                    .children(caret())
                    .when(!after.is_empty(), |field| field.child(after))
            })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn multibyte_boundaries_are_char_safe() {
        let text = "café";
        let previous = text[..text.len()].char_indices().next_back().map(|(i, _)| i);
        assert_eq!(previous, Some(3));
    }
}
