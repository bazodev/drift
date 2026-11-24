//! The bottom player bar.

use std::{cell::Cell, rc::Rc};

use gpui::{
    Bounds, Context, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, SharedString,
    Window, canvas, div, prelude::*, px, relative, rgba,
};

use super::{BG, FAINT, HOVER, LINE, MUTED, RAISED, SURFACE, TEXT, artwork, glow, icon, round_button};
use crate::{
    engine::Command,
    model::{self, PanelTab, ctx, set_like},
};

#[derive(Clone, Copy, PartialEq)]
enum SliderKind {
    Seek,
    Volume,
}

#[derive(Clone, Copy)]
struct Scrub {
    kind: SliderKind,
    fraction: f32,
}

pub struct TransportView {
    scrub: Option<Scrub>,
    seek_bounds: Rc<Cell<Bounds<Pixels>>>,
    volume_bounds: Rc<Cell<Bounds<Pixels>>>,
    _subscriptions: Vec<gpui::Subscription>,
}

pub fn clock(seconds: f64) -> SharedString {
    let total = seconds.max(0.) as u64;
    let (hours, minutes, secs) = (total / 3600, total / 60 % 60, total % 60);
    if hours > 0 { format!("{hours}:{minutes:02}:{secs:02}").into() } else { format!("{minutes}:{secs:02}").into() }
}

impl TransportView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let app = ctx(cx);
        let subscriptions = vec![
            cx.observe(&app.player, |_, _, cx| cx.notify()),
            cx.observe(&app.images, |_, _, cx| cx.notify()),
            cx.observe(&app.ui, |_, _, cx| cx.notify()),
        ];
        Self { scrub: None, seek_bounds: Rc::default(), volume_bounds: Rc::default(), _subscriptions: subscriptions }
    }

    fn fraction_at(&self, kind: SliderKind, position: Point<Pixels>) -> f32 {
        let bounds = match kind {
            SliderKind::Seek => self.seek_bounds.get(),
            SliderKind::Volume => self.volume_bounds.get(),
        };
        if bounds.size.width <= px(0.) {
            return 0.;
        }
        ((position.x - bounds.origin.x) / bounds.size.width).clamp(0., 1.)
    }

    fn apply_live(&self, kind: SliderKind, fraction: f32, cx: &mut Context<Self>) {
        if kind == SliderKind::Volume {
            let level = f64::from(fraction) * 100.;
            let app = ctx(cx);
            app.client.send(Command::Volume(level));
            app.player.update(cx, |player, _| player.state.volume = level);
        }
    }

    fn begin(&mut self, kind: SliderKind, position: Point<Pixels>, cx: &mut Context<Self>) {
        let fraction = self.fraction_at(kind, position);
        self.scrub = Some(Scrub { kind, fraction });
        self.apply_live(kind, fraction, cx);
        cx.notify();
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        let Some(scrub) = self.scrub.take() else { return };
        if scrub.kind == SliderKind::Seek {
            let app = ctx(cx);
            let target = f64::from(scrub.fraction) * app.player.read(cx).state.duration;
            app.client.send(Command::Seek(target));
            app.player.update(cx, |player, _| player.state.position = target);
        }
        cx.notify();
    }

    fn on_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(scrub) = self.scrub else { return };
        if event.pressed_button != Some(MouseButton::Left) {
            self.finish(cx);
            return;
        }
        let fraction = self.fraction_at(scrub.kind, event.position);
        self.scrub = Some(Scrub { fraction, ..scrub });
        self.apply_live(scrub.kind, fraction, cx);
        cx.notify();
    }

    fn on_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.finish(cx);
    }

    fn slider(
        &self,
        id: &'static str,
        kind: SliderKind,
        fraction: f32,
        enabled: bool,
        fill: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let store = match kind {
            SliderKind::Seek => self.seek_bounds.clone(),
            SliderKind::Volume => self.volume_bounds.clone(),
        };
        let active = self.scrub.is_some_and(|scrub| scrub.kind == kind);
        div()
            .id(id)
            .group(id)
            .relative()
            .w_full()
            .h(px(16.))
            .flex()
            .items_center()
            .when(enabled, |slider| {
                slider.cursor_pointer().on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |view, event: &MouseDownEvent, _, cx| {
                        view.begin(kind, event.position, cx);
                    }),
                )
            })
            .child(canvas(move |bounds, _, _| store.set(bounds), |_, _, _, _| {}).absolute().size_full())
            .child(
                div()
                    .w_full()
                    .h(px(4.))
                    .rounded_full()
                    .bg(rgba(RAISED))
                    .child(div().h_full().w(relative(fraction)).rounded_full().bg(fill)),
            )
            .when(enabled, |slider| {
                slider.child(
                    div()
                        .absolute()
                        .left(relative(fraction))
                        .ml(px(-6.))
                        .size(px(12.))
                        .rounded_full()
                        .bg(rgba(TEXT))
                        .when(!active, |thumb| thumb.invisible().group_hover(id, |style| style.visible())),
                )
            })
    }
}

impl Render for TransportView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let app = ctx(cx);
        let state = app.player.read(cx).state.clone();
        let (panel, tint) = {
            let ui = app.ui.read(cx);
            // On "Now" the side column is the panel: while it is put away, no tab button is lit.
            let shown =
                if ui.now_open { ui.prefs.now_column.then(|| ui.panel.unwrap_or(PanelTab::Queue)) } else { ui.panel };
            (shown, ui.accent)
        };
        let can_skip = state.has_track && !state.is_ad;
        let can_seek = can_skip && state.duration > 0.;

        let seek = match self.scrub {
            Some(Scrub { kind: SliderKind::Seek, fraction }) => fraction,
            _ if state.duration > 0. => (state.position / state.duration).clamp(0., 1.) as f32,
            _ => 0.,
        };
        let shown = match self.scrub {
            Some(Scrub { kind: SliderKind::Seek, fraction }) => f64::from(fraction) * state.duration,
            _ => state.position,
        };
        let volume = if state.muted { 0. } else { (state.volume / 100.).clamp(0., 1.) as f32 };
        let video = state.video_id.clone();
        let liked = state.like == "LIKE";
        let disliked = state.like == "DISLIKE";

        let (title, artist): (SharedString, SharedString) = if state.is_ad {
            ("Ad".into(), "Your music will continue after the ad".into())
        } else if state.has_track {
            (state.title.clone().into(), state.artist.clone().into())
        } else {
            ("Nothing playing".into(), "Pick something to play".into())
        };

        let client = app.client.clone();
        let command = move |value: Command| {
            let client = client.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, _: &mut gpui::App| client.send(value)
        };
        let next_repeat: &'static str = match state.repeat.as_str() {
            "NONE" => "ALL",
            "ALL" => "ONE",
            _ => "NONE",
        };
        let repeat_icon = if state.repeat == "ONE" { "repeat_one" } else { "repeat" };
        let repeat_color = if state.repeat == "NONE" { MUTED } else { tint };
        let shuffle_color = if state.shuffle { tint } else { MUTED };
        let skip_color = if can_skip { TEXT } else { FAINT };
        let play_icon = if state.playing { "pause" } else { "play" };
        let volume_icon = if state.muted || state.volume <= 0. { "muted" } else { "volume" };
        let like_video = video.clone();
        let dislike_video = video.clone();

        let play = div()
            .id("play")
            .size(px(48.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(rgba(if state.has_track { TEXT } else { RAISED }))
            .when(state.has_track, |button| {
                button
                    .shadow(glow(tint, 0.55, 26.))
                    .cursor_pointer()
                    .hover(|style| style.opacity(0.88))
                    .on_click(command(Command::Toggle))
            })
            .child(icon(play_icon, 22., if state.has_track { BG } else { FAINT }));

        // The cover opens "Now": the song's galaxy with its queue and lyrics.
        let now_playing = div()
            .id("now-playing")
            .w(px(300.))
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .p(px(4.))
            .rounded(px(18.))
            .when(state.has_track, |row| {
                row.cursor_pointer()
                    .hover(|style| style.bg(rgba(HOVER)))
                    .on_click(|_, _, cx| model::set_now_open(true, cx))
            })
            .child(div().flex_none().rounded(px(12.)).shadow(glow(0x000000ff, 0.6, 18.)).child(artwork(
                &state.artwork,
                56.,
                12.,
                cx,
            )))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().truncate().text_size(px(15.)).font_weight(gpui::FontWeight::SEMIBOLD).child(title))
                    .child(div().truncate().text_sm().text_color(rgba(MUTED)).child(artist)),
            );

        let controls = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(2.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        round_button("shuffle", "shuffle", 36., 18., shuffle_color).on_click(command(Command::Shuffle)),
                    )
                    .child(
                        round_button("previous", "previous", 40., 21., skip_color).on_click(command(Command::Previous)),
                    )
                    .child(play)
                    .child(round_button("next", "next", 40., 21., skip_color).on_click(command(Command::Next)))
                    .child(
                        round_button("repeat", repeat_icon, 36., 18., repeat_color)
                            .on_click(command(Command::Repeat(next_repeat))),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .max_w(px(560.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(rgba(MUTED))
                    .child(div().w(px(40.)).flex().justify_end().child(clock(shown)))
                    .child(div().flex_1().child(self.slider(
                        "seek",
                        SliderKind::Seek,
                        seek,
                        can_seek,
                        rgba(tint).into(),
                        cx,
                    )))
                    .child(div().w(px(40.)).child(clock(state.duration))),
            );

        let ui = app.ui.clone();
        let toggle_panel = move |tab: PanelTab| {
            let ui = ui.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                ui.update(cx, |ui, cx| {
                    if ui.now_open {
                        // On "Now" these buttons show and hide the side column, opened on their tab.
                        let showing = ui.prefs.now_column && ui.panel.unwrap_or(PanelTab::Queue) == tab;
                        ui.prefs.now_column = !showing;
                        ui.prefs.save();
                        ui.panel = Some(tab);
                    } else {
                        ui.panel = if ui.panel == Some(tab) { None } else { Some(tab) };
                    }
                    cx.notify();
                });
            }
        };
        let tab_color = |tab: PanelTab| if panel == Some(tab) { tint } else { MUTED };

        let right = div()
            .w(px(300.))
            .flex_none()
            .flex()
            .items_center()
            .justify_end()
            .gap_1()
            .when(state.has_track && !state.is_ad, |row| {
                row.child(
                    round_button(
                        "dislike",
                        if disliked { "dislike" } else { "dislike_outline" },
                        34.,
                        17.,
                        if disliked { TEXT } else { MUTED },
                    )
                    .on_click(move |_, _, cx| {
                        set_like(&dislike_video, if disliked { "INDIFFERENT" } else { "DISLIKE" }, cx);
                    }),
                )
                .child(
                    round_button(
                        "like",
                        if liked { "like" } else { "like_outline" },
                        34.,
                        17.,
                        if liked { tint } else { MUTED },
                    )
                    .on_click(move |_, _, cx| {
                        set_like(&like_video, if liked { "INDIFFERENT" } else { "LIKE" }, cx);
                    }),
                )
                .child(div().w(px(1.)).h(px(22.)).mx_1().bg(rgba(LINE)))
            })
            .child(
                round_button("open-lyrics", "lyrics", 34., 17., tab_color(PanelTab::Lyrics))
                    .on_click(toggle_panel(PanelTab::Lyrics)),
            )
            .child(
                round_button("open-queue", "queue", 34., 18., tab_color(PanelTab::Queue))
                    .on_click(toggle_panel(PanelTab::Queue)),
            )
            .child(round_button("mute", volume_icon, 34., 18., MUTED).on_click(command(Command::Mute)))
            .child(div().w(px(92.)).child(self.slider(
                "volume",
                SliderKind::Volume,
                volume,
                true,
                rgba(TEXT).into(),
                cx,
            )));

        div()
            .size_full()
            .flex()
            .justify_center()
            .px_5()
            .pt_1()
            .on_mouse_move(cx.listener(Self::on_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_up))
            .child(
                div()
                    .w_full()
                    .max_w(px(1120.))
                    .h(px(84.))
                    .flex()
                    .items_center()
                    .gap_4()
                    .pl(px(10.))
                    .pr_4()
                    .rounded(px(26.))
                    .bg(rgba(SURFACE))
                    .border_1()
                    .border_color(rgba(LINE))
                    .text_color(rgba(TEXT))
                    .child(now_playing)
                    .child(controls)
                    .child(right),
            )
    }
}
