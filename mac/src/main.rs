//! Kitchen Scale: a native macOS app (GPUI) for the Vitamix / Perfect Company Bluetooth scale.

mod ble;

use ble::{Command, Event, Status};
use futures::channel::mpsc::UnboundedSender;
use futures::StreamExt;
use gpui::{
    actions, canvas, div, prelude::*, px, quad, rgb, size, App, Application, Bounds, ClickEvent,
    Context, FocusHandle, Focusable, FontWeight, KeyBinding, Menu, MenuItem, Pixels,
    Rgba, SharedString, Subscription, Task, TitlebarOptions, Window, WindowAppearance, WindowBounds,
    WindowOptions,
};
use std::time::{Duration, Instant};

actions!(kitchen_scale, [Quit, Tare, ClearTare, UnitG, UnitOz, UnitLb]);

// ---------- units ----------
#[derive(Clone, Copy, PartialEq)]
enum Unit {
    G,
    Oz,
    Lb,
}
impl Unit {
    fn grams_per(self) -> f64 {
        match self {
            Unit::G => 1.0,
            Unit::Oz => 28.349523125,
            Unit::Lb => 453.59237,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Unit::G => "g",
            Unit::Oz => "oz",
            Unit::Lb => "lb",
        }
    }
    fn fmt(self, grams: f64) -> String {
        let v = grams / self.grams_per();
        match self {
            Unit::G if v.abs() < 1000.0 => format!("{v:.1}"),
            Unit::G => format!("{v:.0}"),
            Unit::Oz => format!("{v:.2}"),
            Unit::Lb => format!("{v:.3}"),
        }
    }
}

// ---------- theme (same palette as the web app) ----------
#[derive(Clone, Copy)]
struct Theme {
    bg: Rgba,
    surface: Rgba,
    surface2: Rgba,
    ink: Rgba,
    muted: Rgba,
    faint: Rgba,
    line: Rgba,
    accent: Rgba,
    accent_ink: Rgba,
    accent_soft: Rgba,
    warn: Rgba,
    ok: Rgba,
    track: Rgba,
}
impl Theme {
    fn light() -> Self {
        Self {
            bg: rgb(0xf5f1ea),
            surface: rgb(0xfffdf9),
            surface2: rgb(0xefe9df),
            ink: rgb(0x1c1d1b),
            muted: rgb(0x6f6b63),
            faint: rgb(0xa39d92),
            line: rgb(0xe4ddd1),
            accent: rgb(0x2f6f5e),
            accent_ink: rgb(0xffffff),
            accent_soft: rgb(0xdcebe5),
            warn: rgb(0xc0612b),
            ok: rgb(0x2f8a55),
            track: rgb(0xe9e2d6),
        }
    }
    fn dark() -> Self {
        Self {
            bg: rgb(0x141614),
            surface: rgb(0x1c1f1d),
            surface2: rgb(0x252926),
            ink: rgb(0xeef0ec),
            muted: rgb(0xa2a8a1),
            faint: rgb(0x6d736c),
            line: rgb(0x2d322e),
            accent: rgb(0x5cbf9f),
            accent_ink: rgb(0x0c1411),
            accent_soft: rgb(0x1f3a32),
            warn: rgb(0xec935c),
            ok: rgb(0x66c98d),
            track: rgb(0x2a2f2b),
        }
    }
    fn for_appearance(a: WindowAppearance) -> Self {
        match a {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::dark(),
            _ => Self::light(),
        }
    }
}

// ---------- app state ----------
struct ScaleApp {
    status: Status,
    weight: Option<i32>,
    zero: Option<i32>,
    zero_seq: u64,
    battery: Option<u8>,
    unit: Unit,
    target_g: Option<f64>,
    app_tare_g: f64,
    taring: bool,
    last_change: Instant,
    prev_grams: Option<f64>,
    cmd: UnboundedSender<Command>,
    focus: FocusHandle,
    theme: Theme,
    stable_task: Option<Task<()>>,
    _appearance: Subscription,
}

struct Applied {
    changed: bool,
    weight_moved: bool,
}

impl ScaleApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (cmd, mut events) = ble::start();
        // Bluetooth events -> state.
        // Only redraw when something visible changed: the scale repeats identical readings
        // several times a second, and every frame costs GPU memory and power.
        cx.spawn(async move |this, cx| {
            while let Some(ev) = events.next().await {
                let alive = this.update(cx, |s, cx| {
                    let moved = s.apply(ev);
                    if moved.changed {
                        cx.notify();
                    }
                    if moved.weight_moved {
                        s.schedule_stable_repaint(cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        let appearance = cx.observe_window_appearance(window, |s, window, cx| {
            s.theme = Theme::for_appearance(window.appearance());
            cx.notify();
        });
        Self {
            status: Status::Starting,
            weight: None,
            zero: None,
            zero_seq: 0,
            battery: None,
            unit: Unit::G,
            target_g: None,
            app_tare_g: 0.0,
            taring: false,
            last_change: Instant::now(),
            prev_grams: None,
            cmd,
            focus: cx.focus_handle(),
            theme: Theme::for_appearance(window.appearance()),
            stable_task: None,
            _appearance: appearance,
        }
    }

    fn connected(&self) -> bool {
        matches!(self.status, Status::Connected(_))
    }

    /// Gross weight in grams relative to the scale's own zero.
    fn grams(&self) -> Option<f64> {
        if !self.connected() {
            return None;
        }
        Some((self.weight? - self.zero.unwrap_or(0)) as f64 / 1000.0)
    }

    /// Net weight shown on screen (after the app's own tare, with noise near zero hidden).
    fn net(&self) -> Option<f64> {
        let n = self.grams()? - self.app_tare_g;
        Some(if n.abs() < 0.2 { 0.0 } else { n })
    }

    fn apply(&mut self, ev: Event) -> Applied {
        let before = (self.status.clone(), self.weight, self.zero, self.battery, self.taring);
        match ev {
            Event::Status(s) => {
                if !matches!(s, Status::Connected(_)) {
                    self.taring = false;
                }
                if matches!(s, Status::Connecting(_)) {
                    self.weight = None;
                    self.zero = None;
                    self.app_tare_g = 0.0;
                }
                self.status = s;
            }
            Event::Weight(w) => self.weight = Some(w),
            Event::Zero(z) => {
                self.zero = Some(z);
                self.zero_seq += 1;
                if self.taring {
                    self.taring = false;
                    self.app_tare_g = 0.0;
                }
            }
            Event::Battery(b) => self.battery = Some(b),
            Event::Log(m) => eprintln!("[bluetooth] {m}"),
        }
        let mut weight_moved = false;
        if let Some(g) = self.grams() {
            if self.prev_grams.map_or(true, |p| (g - p).abs() > 0.25) {
                self.last_change = Instant::now();
                weight_moved = true;
            }
            self.prev_grams = Some(g);
        }
        let after = (self.status.clone(), self.weight, self.zero, self.battery, self.taring);
        Applied { changed: before != after, weight_moved }
    }

    /// One redraw shortly after the weight stops moving, so "Settling" turns into "Stable".
    /// Replacing the task cancels the previous one.
    fn schedule_stable_repaint(&mut self, cx: &mut Context<Self>) {
        self.stable_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(750)).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        }));
    }

    fn tare(&mut self, cx: &mut Context<Self>) {
        let Some(g) = self.grams() else { return };
        if self.taring {
            return;
        }
        // Ask the scale to zero itself; fall back to the app's own tare if it doesn't confirm.
        self.taring = true;
        let seq = self.zero_seq;
        let near_zero = g.abs() < 0.5;
        let _ = self.cmd.unbounded_send(Command::Tare);
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(if near_zero { 900 } else { 2500 }))
                .await;
            let _ = this.update(cx, |s, cx| {
                if s.taring && s.zero_seq == seq {
                    s.taring = false;
                    if !near_zero {
                        if let Some(g) = s.grams() {
                            s.app_tare_g = g; // scale didn't respond: tare in the app instead
                        }
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn set_unit(&mut self, u: Unit, cx: &mut Context<Self>) {
        self.unit = u;
        cx.notify();
    }

    fn step_target(&mut self, dir: f64, cx: &mut Context<Self>) {
        let step = match self.unit {
            Unit::G => 10.0,
            Unit::Oz => 0.5 * Unit::Oz.grams_per(),
            Unit::Lb => 0.1 * Unit::Lb.grams_per(),
        };
        let cur = self.target_g.unwrap_or(0.0);
        let next = ((cur / step).round() + dir) * step;
        self.target_g = if next > 0.0 { Some(next) } else { None };
        cx.notify();
    }
}

impl Focusable for ScaleApp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

// ---------- small UI helpers ----------
fn button(id: &'static str, label: impl Into<SharedString>, primary: bool, enabled: bool, t: Theme) -> gpui::Stateful<gpui::Div> {
    let (bg, fg) = if primary { (t.accent, t.accent_ink) } else { (t.surface2, t.ink) };
    let mut b = div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .h(px(52.))
        .px_4()
        .rounded(px(14.))
        .bg(bg)
        .text_color(fg)
        .font_weight(FontWeight::SEMIBOLD)
        .child(label.into());
    if enabled {
        b = b.cursor_pointer().hover(|s| s.opacity(0.88)).active(|s| s.opacity(0.75));
    } else {
        b = b.opacity(0.4);
    }
    b
}

fn chip(id: &'static str, label: impl Into<SharedString>, selected: bool, t: Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .h(px(32.))
        .min_w(px(52.))
        .px_3()
        .rounded(px(9.))
        .cursor_pointer()
        .font_weight(FontWeight::SEMIBOLD)
        .text_sm()
        .when(selected, |d| d.bg(t.surface).text_color(t.ink).shadow_sm())
        .when(!selected, |d| d.text_color(t.muted).hover(|s| s.text_color(t.ink)))
        .child(label.into())
}

fn card(t: Theme) -> gpui::Div {
    div().w_full().p_5().rounded(px(20.)).bg(t.surface).border_1().border_color(t.line)
}

impl Render for ScaleApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let unit = self.unit;
        let net = self.net();
        let has = net.is_some();
        let stable = has && self.last_change.elapsed() > Duration::from_millis(700);

        // status pill
        let (dot, status_text): (Rgba, String) = match &self.status {
            Status::Starting => (t.faint, "Starting…".into()),
            Status::NoBluetooth(m) => (t.warn, m.clone()),
            Status::Searching => (t.warn, "Looking for the scale…".into()),
            Status::Connecting(n) => (t.warn, format!("Connecting to {n}…")),
            Status::Connected(n) => (t.ok, n.clone()),
        };

        // target progress
        let pct = match (net, self.target_g) {
            (Some(n), Some(tg)) if tg > 0.0 => (n / tg).max(0.0),
            _ => 0.0,
        };
        let over = pct > 1.02;
        let ring_color = if over { t.warn } else { t.accent };
        let target_text: String = match (net, self.target_g) {
            (Some(n), Some(tg)) => {
                let left = tg - n;
                let tol = (tg * 0.005).max(0.5);
                if left > tol {
                    format!("{} {} to go · {}%", unit.fmt(left), unit.label(), (pct * 100.0).round())
                } else if left < -tol {
                    format!("{} {} over", unit.fmt(-left), unit.label())
                } else {
                    "On target".into()
                }
            }
            (None, Some(_)) => String::new(),
            (_, None) => "Set an amount to see your progress on the ring.".into(),
        };
        let target_color = if over { t.warn } else if target_text == "On target" { t.ok } else { t.muted };

        let sub: String = match (&self.status, net) {
            (Status::Connected(_), None) => "Waiting for the scale…".into(),
            (Status::Connected(_), Some(_)) if self.app_tare_g != 0.0 => format!(
                "App tare {} {} · total {} {}",
                unit.fmt(self.app_tare_g), unit.label(),
                unit.fmt(self.grams().unwrap_or(0.0)), unit.label()
            ),
            (Status::Searching, _) => "Turn the scale on and tap its Bluetooth button.".into(),
            _ => String::new(),
        };

        let number = net.map(|n| unit.fmt(n)).unwrap_or_else(|| "--".into());

        div()
            .key_context("KitchenScale")
            .track_focus(&self.focus)
            .on_action(cx.listener(|s, _: &Tare, _, cx| s.tare(cx)))
            .on_action(cx.listener(|s, _: &ClearTare, _, cx| { s.app_tare_g = 0.0; cx.notify(); }))
            .on_action(cx.listener(|s, _: &UnitG, _, cx| s.set_unit(Unit::G, cx)))
            .on_action(cx.listener(|s, _: &UnitOz, _, cx| s.set_unit(Unit::Oz, cx)))
            .on_action(cx.listener(|s, _: &UnitLb, _, cx| s.set_unit(Unit::Lb, cx)))
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(t.bg)
            .text_color(t.ink)
            // header
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_lg().font_weight(FontWeight::BOLD).child("Kitchen Scale"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_1()
                            .rounded_full()
                            .bg(t.surface)
                            .border_1()
                            .border_color(t.line)
                            .text_xs()
                            .child(div().size(px(8.)).rounded_full().bg(dot))
                            .child(status_text)
                            .when_some(self.battery.filter(|_| self.connected()), |d, b| {
                                d.child(div().text_color(t.muted).child(format!("· {b}%")))
                            }),
                    ),
            )
            // main card: units, ring, readout, tare
            .child(
                card(t)
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .p(px(3.))
                            .rounded(px(12.))
                            .bg(t.surface2)
                            .child(chip("u-g", "g", unit == Unit::G, t).on_click(cx.listener(|s, _: &ClickEvent, _, cx| s.set_unit(Unit::G, cx))))
                            .child(chip("u-oz", "oz", unit == Unit::Oz, t).on_click(cx.listener(|s, _: &ClickEvent, _, cx| s.set_unit(Unit::Oz, cx))))
                            .child(chip("u-lb", "lb", unit == Unit::Lb, t).on_click(cx.listener(|s, _: &ClickEvent, _, cx| s.set_unit(Unit::Lb, cx)))),
                    )
                    .child(
                        div()
                            .relative()
                            .size(px(290.))
                            .child(
                                canvas(
                                    |_, _, _| {},
                                    move |bounds, _, window, _| paint_ring(bounds, pct, t.track, ring_color, window),
                                )
                                .absolute()
                                .size_full(),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .size_full()
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        div()
                                            .text_size(px(76.))
                                            .font_weight(FontWeight::BOLD)
                                            .line_height(px(80.))
                                            .text_color(if has { t.ink } else { t.faint })
                                            .child(number),
                                    )
                                    .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).text_color(t.muted).child(unit.label()))
                                    .child(
                                        div()
                                            .mt_3()
                                            .flex()
                                            .items_center()
                                            .gap_1p5()
                                            .px_2p5()
                                            .py_0p5()
                                            .rounded_full()
                                            .text_xs()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .bg(if stable { t.accent_soft } else { t.surface2 })
                                            .text_color(if stable { t.ok } else { t.muted })
                                            .child(div().size(px(7.)).rounded_full().bg(if stable { t.ok } else { t.faint }))
                                            .child(if !has { "Waiting" } else if self.taring { "Taring…" } else if stable { "Stable" } else { "Settling" }),
                                    ),
                            ),
                    )
                    .child(div().h(px(18.)).text_sm().text_color(t.muted).child(sub))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .gap_2p5()
                            .child(
                                button("tare", if self.taring { "Taring…" } else { "Tare" }, true, has && !self.taring, t)
                                    .flex_grow()
                                    .text_lg()
                                    .on_click(cx.listener(|s, _: &ClickEvent, _, cx| s.tare(cx))),
                            )
                            .child(
                                button("clear", "Clear tare", false, has && self.app_tare_g != 0.0, t)
                                    .w(px(130.))
                                    .on_click(cx.listener(|s, _: &ClickEvent, _, cx| { s.app_tare_g = 0.0; cx.notify(); })),
                            ),
                    ),
            )
            // target card
            .child(
                card(t)
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(t.muted).child("Target amount"))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(button("t-minus", "−", false, self.target_g.is_some(), t).w(px(44.)).h(px(40.))
                                        .on_click(cx.listener(|s, _: &ClickEvent, _, cx| s.step_target(-1.0, cx))))
                                    .child(
                                        div()
                                            .min_w(px(110.))
                                            .text_center()
                                            .text_lg()
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(if self.target_g.is_some() { t.ink } else { t.faint })
                                            .child(match self.target_g {
                                                Some(tg) => format!("{} {}", unit.fmt(tg), unit.label()),
                                                None => "None".into(),
                                            }),
                                    )
                                    .child(button("t-plus", "+", false, true, t).w(px(44.)).h(px(40.))
                                        .on_click(cx.listener(|s, _: &ClickEvent, _, cx| s.step_target(1.0, cx)))),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .children([(100.0, "p100"), (250.0, "p250"), (500.0, "p500"), (1000.0, "p1000")].map(|(g, id)| {
                                button(id, format!("{} {}", Unit::G.fmt(g).trim_end_matches(".0"), "g"), false, true, t)
                                    .flex_1()
                                    .h(px(36.))
                                    .text_sm()
                                    .on_click(cx.listener(move |s, _: &ClickEvent, _, cx| { s.target_g = Some(g); cx.notify(); }))
                            }))
                            .child(button("p-clear", "Clear", false, self.target_g.is_some(), t).h(px(36.)).text_sm()
                                .on_click(cx.listener(|s, _: &ClickEvent, _, cx| { s.target_g = None; cx.notify(); }))),
                    )
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(target_color).child(target_text)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(t.faint)
                    .text_center()
                    .child("T or Space to tare · G / O / L to change units"),
            )
    }
}

/// Draw the target ring: a full track plus an arc from 12 o'clock for the progress.
/// Built from overlapping round quads (GPUI's cheapest primitive) rather than a stroked
/// path, which would go through the much heavier MSAA path renderer.
fn paint_ring(bounds: Bounds<Pixels>, pct: f64, track: Rgba, color: Rgba, window: &mut Window) {
    const THICK: f32 = 10.0;
    let c = bounds.center();
    let r = f32::from(bounds.size.width.min(bounds.size.height)) / 2.0 - THICK;
    let dots = ((std::f32::consts::TAU * r) / 2.5).ceil() as usize; // ~2.5 px apart: reads as a solid line
    let filled = ((pct.clamp(0.0, 1.0) as f32) * dots as f32).round() as usize;
    for i in 0..dots {
        let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * i as f32 / dots as f32;
        let (x, y) = (c.x + px(r * a.cos() - THICK / 2.0), c.y + px(r * a.sin() - THICK / 2.0));
        let b = Bounds { origin: gpui::point(x, y), size: size(px(THICK), px(THICK)) };
        let col = if i < filled { color } else { track };
        window.paint_quad(quad(b, px(THICK / 2.0), col, px(0.), gpui::transparent_black(), Default::default()));
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("t", Tare, Some("KitchenScale")),
            KeyBinding::new("space", Tare, Some("KitchenScale")),
            KeyBinding::new("escape", ClearTare, Some("KitchenScale")),
            KeyBinding::new("g", UnitG, Some("KitchenScale")),
            KeyBinding::new("o", UnitOz, Some("KitchenScale")),
            KeyBinding::new("l", UnitLb, Some("KitchenScale")),
        ]);
        cx.set_menus(vec![Menu {
            name: "Kitchen Scale".into(),
            items: vec![MenuItem::action("Quit Kitchen Scale", Quit)],
        }]);
        let bounds = Bounds::centered(None, size(px(440.), px(760.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Kitchen Scale".into()),
                        ..Default::default()
                    }),
                    window_min_size: Some(size(px(400.), px(700.))),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| ScaleApp::new(window, cx)),
            )
            .expect("open window");
        window
            .update(cx, |view, window, cx| {
                window.focus(&view.focus_handle(cx));
                cx.activate(true);
            })
            .ok();
        // Quit when the window is closed.
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
    });
}
