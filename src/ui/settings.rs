//! Native settings: Drift's own options plus the account's YouTube Music settings.

use gpui::{AnyElement, Context, Subscription, Task, Window, div, prelude::*, px, rgba};
use serde_json::{Value, json};

use super::{FAINT, HOVER, LINE, MUTED, RAISED, SURFACE, TEXT, icon, pill};
use crate::{
    model::{Modal, ctx, open_menu, toast},
    ytm::{self, Action, MenuEntry, SettingCategory, SettingItem, SettingOption},
};

enum Loaded {
    Idle,
    Loading,
    Ready(Vec<SettingCategory>),
    Failed(String),
}

pub struct SettingsView {
    state: Loaded,
    revision: u64,
    task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

fn switch(on: bool, tint: u32) -> impl IntoElement {
    div()
        .w(px(44.))
        .h(px(26.))
        .flex_none()
        .flex()
        .items_center()
        .p(px(4.))
        .rounded_full()
        .bg(rgba(if on { tint } else { RAISED }))
        .when(on, |track| track.justify_end())
        .child(div().size(px(18.)).rounded_full().bg(rgba(TEXT)))
}

fn pref_row(
    key: usize,
    title: &str,
    summary: &str,
    on: bool,
    tint: u32,
    set: impl Fn(&mut crate::prefs::Prefs, bool) + 'static,
) -> AnyElement {
    row(("drift-pref", key), title.to_string(), summary.to_string())
        .cursor_pointer()
        .hover(|style| style.bg(rgba(HOVER)))
        .on_click(move |_, _, cx| {
            ctx(cx).ui.update(cx, |ui, cx| {
                set(&mut ui.prefs, !on);
                ui.prefs.save();
                cx.notify();
            });
        })
        .child(switch(on, tint))
        .into_any_element()
}

fn row(id: (&'static str, usize), title: String, summary: String) -> gpui::Stateful<gpui::Div> {
    div().id(id).min_h(px(64.)).w_full().flex().items_center().gap_4().px_5().py_3().child(
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
            .when(!summary.is_empty(), |column| {
                column.child(div().text_xs().line_height(px(18.)).text_color(rgba(MUTED)).child(summary))
            }),
    )
}

fn card(title: String, rows: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .pl_1()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgba(FAINT))
                .child(title.to_uppercase()),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .rounded(px(20.))
                .overflow_hidden()
                .bg(rgba(SURFACE))
                .border_1()
                .border_color(rgba(LINE))
                .children(rows),
        )
        .into_any_element()
}

impl SettingsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let app = ctx(cx);
        let subscriptions = vec![cx.observe(&app.ui, |this, ui, cx| {
            let (open, revision) = {
                let ui = ui.read(cx);
                (ui.settings_open, ui.settings_revision)
            };
            if !open {
                this.state = Loaded::Idle;
                this.task = None;
            } else if matches!(this.state, Loaded::Idle) || revision != this.revision {
                this.load(cx);
            }
            this.revision = revision;
            cx.notify();
        })];
        Self { state: Loaded::Idle, revision: 0, task: None, _subscriptions: subscriptions }
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.state = Loaded::Loading;
        let call = ctx(cx).client.api("account/get_setting", json!({}));
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |view, cx| {
                view.state = match result {
                    Ok(json) => Loaded::Ready(ytm::parse_settings(&json)),
                    Err(error) => Loaded::Failed(error),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Applies one setting. Cookie-based ones reload the player, so they ask first.
    fn apply(&mut self, label: String, endpoint: Value, cx: &mut Context<Self>) {
        if ytm::reloads_player(&endpoint) {
            ctx(cx).ui.update(cx, |ui, cx| {
                ui.menu = None;
                ui.modal = Some(Modal::Confirm {
                    label: format!("{label}: the player will reload and the music will stop"),
                    action: Action::Service(endpoint),
                });
                cx.notify();
            });
            return;
        }
        let call = ctx(cx).client.service(&endpoint);
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(_) => toast("Setting saved", cx),
                    Err(error) => toast(format!("Something went wrong: {error}"), cx),
                }
                view.load(cx);
            });
        }));
    }

    /// Debug only: logs the loaded structure, without any values from the account.
    #[cfg(debug_assertions)]
    pub fn report(&self, cx: &gpui::App) {
        let app = ctx(cx);
        let state = &app.player.read(cx).state;
        let Loaded::Ready(categories) = &self.state else {
            eprintln!("settings: not loaded");
            return;
        };
        let groups: Vec<String> = categories
            .iter()
            .map(|category| {
                let choices = category.items.iter().filter(|item| matches!(item, SettingItem::Choice { .. })).count();
                let resolved = category
                    .items
                    .iter()
                    .filter(|item| {
                        matches!(item, SettingItem::Choice { options, .. } if options.iter().any(|option| {
                            option.selected || ytm::is_current_locale(&option.endpoint, &state.language, &state.region)
                        }))
                    })
                    .count();
                format!("{}:{} choices {resolved}/{choices}", category.items.len(), category.title)
            })
            .collect();
        eprintln!("settings: {groups:?}");
    }

    fn drift_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let (prefs, tint) = {
            let ui = ctx(cx).ui.read(cx);
            (ui.prefs.clone(), ui.accent)
        };
        let rows = vec![
            pref_row(
                6,
                "Galaxy in the window",
                "The song that is playing also becomes a galaxy behind Drift's interface. Turn it off for a calm dark background.",
                prefs.window_galaxy,
                tint,
                |prefs, on| prefs.window_galaxy = on,
            ),
            pref_row(
                7,
                "Music video as stars",
                "When a song has a music video, Drift plays the video instead and builds its picture from the galaxy's stars. The sound comes from the video, and it uses more data.",
                prefs.clip_particles,
                tint,
                |prefs, on| prefs.clip_particles = on,
            ),
            pref_row(
                0,
                "Synced lyrics",
                "Highlights the line being sung and spells it out in the galaxy's stars, word by word. Unless a music video brings its own subtitles, the names of playing tracks are sent to LRCLIB, an open lyrics catalog.",
                prefs.synced_lyrics,
                tint,
                |prefs, on| prefs.synced_lyrics = on,
            ),
            pref_row(
                1,
                "Island in the notch",
                "Cover art and a wave that moves with the music, right in the MacBook's notch.",
                prefs.live_island,
                tint,
                |prefs, on| prefs.live_island = on,
            ),
            pref_row(
                2,
                "Live wallpaper",
                "While music plays, every song becomes its own galaxy, made from its cover. Your real wallpaper stays as it is.",
                prefs.live_wallpaper,
                tint,
                |prefs, on| prefs.live_wallpaper = on,
            ),
            pref_row(
                3,
                "Island glow",
                "A soft glow around the island in the colors of the cover.",
                prefs.island_glow,
                tint,
                |prefs, on| prefs.island_glow = on,
            ),
            pref_row(
                4,
                "Island pulse",
                "The island grows a little and brightens with the voice.",
                prefs.island_pulse,
                tint,
                |prefs, on| prefs.island_pulse = on,
            ),
            pref_row(
                5,
                "Chorus burst",
                "Drift finds the chorus from repeats in the lyrics. When it comes, the galaxy opens up and speeds up, and the words grow bigger.",
                prefs.chorus_burst,
                tint,
                |prefs, on| prefs.chorus_burst = on,
            ),
        ];
        card("Drift".to_string(), rows)
    }

    fn item(
        &self,
        item: &SettingItem,
        key: usize,
        safety: bool,
        locale: (&str, &str),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match item {
            SettingItem::Toggle { title, summary, enabled, enable, disable } => {
                let on = if ytm::is_safety_toggle(enable) { safety } else { *enabled };
                let tint = ctx(cx).ui.read(cx).accent;
                let endpoint = if on { disable.clone() } else { enable.clone() };
                let label = title.clone();
                row(("setting", key), title.clone(), summary.clone())
                    .cursor_pointer()
                    .hover(|style| style.bg(rgba(HOVER)))
                    .on_click(cx.listener(move |view, _, _, cx| view.apply(label.clone(), endpoint.clone(), cx)))
                    .child(switch(on, tint))
                    .into_any_element()
            }
            SettingItem::Choice { title, summary, options } => {
                let (language, region) = locale;
                let is_current = |option: &SettingOption| {
                    option.selected || ytm::is_current_locale(&option.endpoint, language, region)
                };
                let current = options
                    .iter()
                    .find(|option| is_current(option))
                    .map_or_else(|| "Choose".to_string(), |option| option.name.clone());
                let entries: Vec<MenuEntry> = options
                    .iter()
                    .map(|option| MenuEntry {
                        label: option.name.clone(),
                        icon: if is_current(option) { "CHECK".into() } else { "BLANK".into() },
                        action: Action::Service(option.endpoint.clone()),
                        // Language and country reload the player, so they ask first.
                        destructive: ytm::reloads_player(&option.endpoint),
                    })
                    .collect();
                let menu_title = title.clone();
                row(("setting", key), title.clone(), summary.clone())
                    .cursor_pointer()
                    .hover(|style| style.bg(rgba(HOVER)))
                    .on_click(move |_, window, cx| {
                        open_menu(menu_title.clone(), entries.clone(), None, window, cx);
                    })
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_sm()
                            .text_color(rgba(MUTED))
                            .child(current)
                            .child(icon("down", 14., MUTED)),
                    )
                    .into_any_element()
            }
            SettingItem::Action { title, summary, endpoint, confirm } => {
                let (label, action) = (
                    if confirm.is_empty() { title.clone() } else { format!("{title}. {confirm}") },
                    Action::Service(endpoint.clone()),
                );
                row(("setting", key), title.clone(), summary.clone())
                    .cursor_pointer()
                    .hover(|style| style.bg(rgba(HOVER)))
                    .on_click(move |_, _, cx| {
                        let (label, action) = (label.clone(), action.clone());
                        ctx(cx).ui.update(cx, |ui, cx| {
                            ui.modal = Some(Modal::Confirm { label, action });
                            cx.notify();
                        });
                    })
                    .child(icon("forward", 16., FAINT))
                    .into_any_element()
            }
            SettingItem::Info { title, summary, url } => {
                let base = row(("setting", key), title.clone(), summary.clone());
                match url.clone() {
                    Some(url) => base
                        .cursor_pointer()
                        .hover(|style| style.bg(rgba(HOVER)))
                        .on_click(move |_, _, cx| cx.open_url(&url))
                        .child(icon("forward", 16., FAINT))
                        .into_any_element(),
                    None => base.into_any_element(),
                }
            }
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (safety, language, region) = {
            let app = ctx(cx);
            let state = &app.player.read(cx).state;
            (state.safety_mode, state.language.clone(), state.region.clone())
        };
        let drift = self.drift_section(cx);
        let account: AnyElement = match &self.state {
            Loaded::Ready(categories) => {
                let categories = categories.clone();
                let cards: Vec<AnyElement> = categories
                    .iter()
                    .enumerate()
                    .map(|(group, category)| {
                        let rows: Vec<AnyElement> = category
                            .items
                            .iter()
                            .enumerate()
                            .map(|(index, item)| self.item(item, group * 100 + index, safety, (&language, &region), cx))
                            .collect();
                        card(category.title.clone(), rows)
                    })
                    .collect();
                div().flex().flex_col().gap_6().children(cards).into_any_element()
            }
            Loaded::Failed(error) => div()
                .flex()
                .flex_col()
                .items_start()
                .gap_3()
                .text_sm()
                .text_color(rgba(MUTED))
                .child(format!("Couldn't load settings: {error}"))
                .child(
                    pill("settings-retry", None, "Try again", true)
                        .on_click(cx.listener(|view, _, _, cx| view.load(cx))),
                )
                .into_any_element(),
            _ => div().text_sm().text_color(rgba(MUTED)).child("Loading settings…").into_any_element(),
        };
        div().id("settings").size_full().overflow_y_scroll().text_color(rgba(TEXT)).child(
            div()
                .w_full()
                .max_w(px(760.))
                .mx_auto()
                .px_8()
                .pb_10()
                .flex()
                .flex_col()
                .gap_6()
                .child(div().pt_2().text_size(px(44.)).font_weight(gpui::FontWeight::BOLD).child("Settings"))
                .child(drift)
                .child(account),
        )
    }
}
