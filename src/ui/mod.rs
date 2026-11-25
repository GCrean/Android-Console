//! GPUI views: status bar, terminal viewport, scrollbar, input handling.

mod keys;
mod paint;

use std::time::Duration;

use gpui::{
    canvas, div, point, prelude::*, px, rgb, rgba, size, Bounds, ClipboardItem, Context,
    FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Render, ScrollWheelEvent, SharedString, Size, Stateful, Window,
};

use crate::adb::{self, AdbError, Device, Event, Session};
use crate::term::{Pos, Term};
use crate::settings;
use paint::{CursorShape, RenderStyle};

pub const SCROLLBACK_LINES: usize = 10_000;
const PAD: f32 = 8.0;
const SCROLLBAR_W: f32 = 12.0;

const C_BAR: u32 = 0x161b22;
const C_BORDER: u32 = 0x30363d;
const C_BTN: u32 = 0x21262d;
const C_BTN_HOVER: u32 = 0x30363d;
const C_TEXT: u32 = 0xe6edf3;
const C_MUTED: u32 = 0x8b949e;
const C_GREEN: u32 = 0x3fb950;
const C_AMBER: u32 = 0xd29922;
const C_RED: u32 = 0xf85149;
const C_ACCENT: u32 = 0x1f6feb;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConnState {
    Disconnected,
    Connecting,
    Connected,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Drag {
    None,
    Select,
    Scrollbar,
}

pub struct ConsoleView {
    focus: FocusHandle,
    addr_focus: FocusHandle,
    term: Term,
    state: ConnState,
    session: Option<Session>,
    /// Bumped on every connect/disconnect so stale event pumps stop.
    session_id: u64,
    got_data: bool,
    devices: Vec<Device>,
    selected: Option<String>,
    active: Option<Device>,
    picker_open: bool,
    addr: String,
    notice: Option<String>,
    font_size: f32,
    cursor_shape: CursorShape,
    blink: bool,
    blink_on: bool,
    selection: Option<(Pos, Pos)>,
    anchor: Pos,
    drag: Drag,
    /// Terminal area (incl. padding) and the cell grid origin from the last frame.
    area: Bounds<Pixels>,
    grid_origin: gpui::Point<Pixels>,
    cell: Size<Pixels>,
    scroll_accum: f32,
}

impl ConsoleView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        let mut term = Term::new(80, 24, SCROLLBACK_LINES);
        term.feed(
            b"\x1b[1;38;2;88;166;255mAndroid Console\x1b[0m  \x1b[2m(ADB shell over GPUI)\x1b[0m\r\n\
              Pick a device and press \x1b[1;32mConnect\x1b[0m, or use Wi-Fi: type host:port and press Enter.\r\n\
              \x1b[2mScroll: wheel / Shift+PgUp/PgDn / scrollbar. Copy: Ctrl+Shift+C. Paste: Ctrl+Shift+V.\x1b[0m\r\n\r\n",
        );

        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(530)).await;
            let alive = this.update(cx, |t, cx| {
                if t.blink {
                    t.blink_on = !t.blink_on;
                    cx.notify();
                }
            });
            if alive.is_err() {
                break;
            }
        })
        .detach();

        let mut this = ConsoleView {
            focus,
            addr_focus: cx.focus_handle(),
            term,
            state: ConnState::Disconnected,
            session: None,
            session_id: 0,
            got_data: false,
            devices: Vec::new(),
            selected: None,
            active: None,
            picker_open: false,
            addr: String::new(),
            notice: None,
            font_size: settings::Settings::load().font_size,
            cursor_shape: CursorShape::Block,
            blink: true,
            blink_on: true,
            selection: None,
            anchor: Pos { line: 0, col: 0 },
            drag: Drag::None,
            area: Bounds::default(),
            grid_origin: point(px(0.), px(0.)),
            cell: size(px(8.), px(18.)),
            scroll_accum: 0.0,
        };
        this.refresh_devices(cx);
        this
    }

    // ---- connection lifecycle -----------------------------------------

    fn note_term(&mut self, color: u8, msg: &str) {
        let text = msg.replace("\r\n", "\n").replace('\n', "\r\n");
        self.term.feed(format!("\x1b[{color}m{text}\x1b[0m\r\n").as_bytes());
    }

    fn fail(&mut self, msg: String, cx: &mut Context<Self>) {
        self.state = ConnState::Disconnected;
        self.session = None;
        self.active = None;
        self.note_term(31, &msg);
        self.notice = Some(msg);
        cx.notify();
    }

    fn refresh_devices(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let res = cx.background_executor().spawn(async { adb::list_devices() }).await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(devs) => {
                        this.devices = devs;
                        if this.selected.is_none() {
                            let mut ready = this.devices.iter().filter(|d| d.is_ready());
                            if let (Some(d), None) = (ready.next(), ready.next()) {
                                this.selected = Some(d.serial.clone());
                            }
                        }
                    }
                    Err(e) => {
                        this.devices.clear();
                        this.notice = Some(e.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        if self.state != ConnState::Disconnected {
            return;
        }
        self.state = ConnState::Connecting;
        self.notice = None;
        self.picker_open = false;
        let want = self.selected.clone();
        cx.spawn(async move |this, cx| {
            let res = cx.background_executor().spawn(async { adb::list_devices() }).await;
            this.update(cx, |this, cx| this.start_session(res, want, cx)).ok();
        })
        .detach();
        cx.notify();
    }

    fn start_session(
        &mut self,
        res: Result<Vec<Device>, AdbError>,
        want: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.state != ConnState::Connecting {
            return; // cancelled meanwhile
        }
        let devs = match res {
            Ok(d) => d,
            Err(e) => return self.fail(e.to_string(), cx),
        };
        self.devices = devs;

        let chosen = match want.as_ref().and_then(|w| self.devices.iter().find(|d| &d.serial == w)) {
            Some(d) if d.is_ready() => Ok(d.clone()),
            Some(d) => Err(format!(
                "Device {} is '{}'.{}",
                d.serial,
                d.state,
                if d.state == "unauthorized" {
                    " Accept the USB debugging prompt on the device."
                } else {
                    ""
                }
            )),
            None => {
                let ready: Vec<&Device> = self.devices.iter().filter(|d| d.is_ready()).collect();
                match ready.len() {
                    1 => Ok(ready[0].clone()),
                    0 if self.devices.is_empty() => Err(
                        "No devices found. Enable USB debugging and plug in a device, or connect over Wi-Fi."
                            .to_string(),
                    ),
                    0 => Err("No authorized device: check the debugging prompt on the device.".to_string()),
                    _ => {
                        self.picker_open = true;
                        Err("Multiple devices attached: choose one from the device list.".to_string())
                    }
                }
            }
        };
        let dev = match chosen {
            Ok(d) => d,
            Err(m) => return self.fail(m, cx),
        };

        match Session::start(Some(&dev.serial)) {
            Ok((session, rx)) => {
                self.session_id += 1;
                let id = self.session_id;
                self.session = Some(session);
                self.got_data = false;
                self.selected = Some(dev.serial.clone());
                self.note_term(2, &format!("[connecting to {} via {}]", dev.display_name(), dev.transport.label()));
                self.active = Some(dev);
                cx.spawn(async move |this, cx| {
                    while let Ok(first) = rx.recv().await {
                        let mut batch = vec![first];
                        while batch.len() < 256 {
                            match rx.try_recv() {
                                Ok(e) => batch.push(e),
                                Err(_) => break,
                            }
                        }
                        let alive = this.update(cx, |this, cx| {
                            if this.session_id != id {
                                return false;
                            }
                            for e in batch {
                                this.on_event(e);
                            }
                            cx.notify();
                            true
                        });
                        if !matches!(alive, Ok(true)) {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(e) => self.fail(e.to_string(), cx),
        }
        cx.notify();
    }

    fn on_event(&mut self, ev: Event) {
        match ev {
            Event::Data(bytes) => {
                if !self.got_data {
                    self.got_data = true;
                    self.state = ConnState::Connected;
                    self.notice = None;
                }
                self.term.feed(&bytes);
                let reply = self.term.take_responses();
                if !reply.is_empty() {
                    if let Some(s) = &self.session {
                        s.send(reply);
                    }
                }
            }
            Event::Stderr(text) => {
                let t = text.trim().to_string();
                if !t.is_empty() {
                    self.note_term(31, &t);
                    self.notice = Some(t);
                }
            }
            Event::Error(msg) => {
                self.note_term(31, &msg);
                self.notice = Some(msg);
            }
            Event::Exited(code) => {
                let was_live = self.got_data;
                let code_txt = code.map(|c| format!(" (code {c})")).unwrap_or_default();
                self.note_term(33, &format!("[adb shell exited{code_txt}]"));
                if !was_live && self.notice.is_none() {
                    self.notice = Some(format!("adb exited before the shell started{code_txt}"));
                }
                self.session_id += 1;
                self.session = None;
                self.active = None;
                self.state = ConnState::Disconnected;
            }
        }
    }

    /// Tear down the session; dropping it kills and reaps the `adb` child.
    fn disconnect_now(&mut self) {
        self.session_id += 1;
        self.session = None;
        self.active = None;
        self.state = ConnState::Disconnected;
    }

    /// Called when the window closes.
    pub fn shutdown(&mut self) {
        self.disconnect_now();
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        if self.state != ConnState::Disconnected {
            self.disconnect_now();
            self.note_term(33, "[disconnected]");
            cx.notify();
        }
    }

    fn connect_wifi(&mut self, cx: &mut Context<Self>) {
        let addr = self.addr.trim().to_string();
        if addr.is_empty() || self.state != ConnState::Disconnected {
            return;
        }
        self.state = ConnState::Connecting;
        self.notice = None;
        self.note_term(2, &format!("[adb connect {addr}]"));
        cx.spawn(async move |this, cx| {
            let a = addr.clone();
            let res = cx.background_executor().spawn(async move { adb::connect_tcp(&a) }).await;
            this.update(cx, |this, cx| match res {
                Ok(msg) => {
                    this.note_term(2, &format!("[{msg}]"));
                    this.state = ConnState::Disconnected;
                    this.selected = Some(if addr.contains(':') { addr } else { format!("{addr}:5555") });
                    this.connect(cx);
                }
                Err(e) => this.fail(e.to_string(), cx),
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    // ---- terminal sizing -----------------------------------------------

    fn apply_resize(&mut self, cols: usize, rows: usize, cx: &mut Context<Self>) {
        self.term.resize(cols, rows);
        cx.notify();
    }

    fn prepare(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) -> paint::Frame {
        let font_size = px(self.font_size);
        let (cw, ch) = paint::cell_metrics(window, font_size);
        self.cell = size(cw, ch);
        self.area = bounds;
        let pad = px(PAD);
        let origin = point(bounds.left() + pad, bounds.top() + pad);
        self.grid_origin = origin;
        let inner_w = (bounds.size.width - pad * 2. - px(SCROLLBAR_W)).max(cw * 2.);
        let inner_h = (bounds.size.height - pad * 2.).max(ch);
        let cols = ((inner_w / cw).floor() as usize).max(2);
        let rows = ((inner_h / ch).floor() as usize).max(1);
        if cols != self.term.cols() || rows != self.term.rows() {
            self.apply_resize(cols, rows, cx);
        }
        let style = RenderStyle {
            font_size,
            cell_w: cw,
            cell_h: ch,
            cursor_shape: self.cursor_shape,
            cursor_on: !self.blink || self.blink_on,
            focused: self.focus.is_focused(window),
        };
        paint::build_frame(&self.term, self.selection, origin, &style, window)
    }

    // ---- selection / clipboard ----------------------------------------

    fn cell_at(&self, p: gpui::Point<Pixels>) -> (usize, usize) {
        let x = ((p.x - self.grid_origin.x) / self.cell.width).floor().max(0.0) as usize;
        let y = ((p.y - self.grid_origin.y) / self.cell.height).floor().max(0.0) as usize;
        (y.min(self.term.rows() - 1), x.min(self.term.cols() - 1))
    }

    fn pos_at(&self, p: gpui::Point<Pixels>) -> Pos {
        let (row, col) = self.cell_at(p);
        Pos { line: self.term.abs_line(row), col }
    }

    fn selected_text(&self) -> Option<String> {
        let (a, b) = self.selection?;
        let t = self.term.text_range(a, b);
        (!t.is_empty()).then_some(t)
    }

    fn copy_selection(&self, cx: &mut Context<Self>) {
        if let Some(t) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(t));
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) else { return };
        if let Some(s) = &self.session {
            self.term.scroll_to_bottom();
            s.send(text.replace("\r\n", "\n").replace('\r', "\n").into_bytes());
        }
    }

    // ---- input handlers -------------------------------------------------

    fn key_down(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        let m = k.modifiers;
        let key = k.key.as_str();
        cx.stop_propagation();

        let copy = (m.control && m.shift && key == "c") || (m.platform && key == "c") || (m.control && key == "insert");
        let paste = (m.control && m.shift && key == "v") || (m.platform && key == "v") || (m.shift && !m.control && key == "insert");
        if copy {
            return self.copy_selection(cx);
        }
        if paste {
            return self.paste(cx);
        }
        if (m.control || m.platform) && !m.alt {
            match key {
                "=" | "+" => return self.set_font_size(self.font_size + 1.0, cx),
                "-" => return self.set_font_size(self.font_size - 1.0, cx),
                "0" => return self.set_font_size(settings::DEFAULT_FONT_SIZE, cx),
                _ => {}
            }
        }
        if m.shift && !m.control && !m.alt {
            let page = self.term.rows().saturating_sub(1).max(1) as isize;
            let handled = match key {
                "pageup" => {
                    self.term.scroll_view(page);
                    true
                }
                "pagedown" => {
                    self.term.scroll_view(-page);
                    true
                }
                "home" => {
                    self.term.scroll_to_offset(usize::MAX);
                    true
                }
                "end" => {
                    self.term.scroll_to_bottom();
                    true
                }
                _ => false,
            };
            if handled {
                cx.notify();
                return;
            }
        }
        if let Some(bytes) = keys::encode_key(k) {
            if let Some(s) = &self.session {
                self.term.scroll_to_bottom();
                s.send(bytes);
            }
            self.blink_on = true;
            cx.notify();
        }
    }

    fn addr_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        cx.stop_propagation();
        match k.key.as_str() {
            "enter" => self.connect_wifi(cx),
            "escape" => window.focus(&self.focus),
            "backspace" => {
                self.addr.pop();
            }
            "v" if k.modifiers.control || k.modifiers.platform => {
                if let Some(t) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    self.addr.push_str(t.trim());
                }
            }
            _ => {
                if !k.modifiers.control && !k.modifiers.platform {
                    if let Some(c) = &k.key_char {
                        if c.chars().all(|c| !c.is_control() && !c.is_whitespace()) {
                            self.addr.push_str(c);
                        }
                    }
                }
            }
        }
        cx.notify();
    }

    fn set_font_size(&mut self, v: f32, cx: &mut Context<Self>) {
        self.font_size = settings::clamp_font_size(v);
        if let Err(e) = (settings::Settings { font_size: self.font_size }).save() {
            self.notice = Some(format!("Could not save settings: {e}"));
        }
        cx.notify();
    }

    fn mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        self.picker_open = false;
        self.anchor = self.pos_at(ev.position);
        self.selection = None;
        self.drag = Drag::Select;
        cx.notify();
    }

    fn mouse_move(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        match self.drag {
            Drag::None => {}
            Drag::Select => {
                // Dragging past an edge scrolls the history.
                if ev.position.y < self.grid_origin.y {
                    self.term.scroll_view(1);
                } else if ev.position.y > self.area.bottom() - px(PAD) {
                    self.term.scroll_view(-1);
                }
                let pos = self.pos_at(ev.position);
                self.selection = (pos != self.anchor).then_some((self.anchor, pos));
                cx.notify();
            }
            Drag::Scrollbar => {
                self.scrollbar_to(ev.position.y);
                cx.notify();
            }
        }
    }

    fn scrollbar_to(&mut self, y: Pixels) {
        let h = self.area.size.height.max(px(1.));
        let frac = ((y - self.area.top()) / h).clamp(0.0, 1.0);
        let sb = self.term.scrollback_len() as f32;
        self.term.scroll_to_offset(((1.0 - frac) * sb).round() as usize);
    }

    fn scroll_wheel(&mut self, ev: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let dy = ev.delta.pixel_delta(self.cell.height).y;
        self.scroll_accum += dy / self.cell.height;
        let lines = self.scroll_accum.trunc();
        if lines != 0.0 {
            self.scroll_accum -= lines;
            self.term.scroll_view(lines as isize);
            cx.notify();
        }
    }

    // ---- rendering --------------------------------------------------------

    fn button(&self, id: &'static str, label: impl Into<SharedString>, enabled: bool) -> Stateful<gpui::Div> {
        let b = div()
            .id(id)
            .h(px(26.))
            .px_3()
            .flex()
            .items_center()
            .rounded_md()
            .border_1()
            .border_color(rgb(C_BORDER))
            .bg(rgb(C_BTN))
            .text_color(rgb(C_TEXT))
            .text_sm()
            .child(label.into());
        if enabled {
            b.cursor_pointer().hover(|s| s.bg(rgb(C_BTN_HOVER)))
        } else {
            b.opacity(0.45)
        }
    }

    fn status_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (dot, label): (u32, &str) = match self.state {
            ConnState::Disconnected => (C_RED, "Disconnected"),
            ConnState::Connecting => (C_AMBER, "Connecting…"),
            ConnState::Connected => (C_GREEN, "Connected"),
        };
        let target: SharedString = match (&self.active, &self.selected) {
            (Some(d), _) => format!("{} · {}", d.display_name(), d.transport.label()).into(),
            (None, Some(s)) => s.clone().into(),
            (None, None) => "no device".into(),
        };
        let idle = self.state == ConnState::Disconnected;
        let live = self.state != ConnState::Disconnected;
        let addr_focused = self.addr_focus.is_focused(window);
        let addr_text: SharedString = if self.addr.is_empty() && !addr_focused {
            "host:port".into()
        } else if addr_focused {
            format!("{}▏", self.addr).into()
        } else {
            self.addr.clone().into()
        };

        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .bg(rgb(C_BAR))
            .border_b_1()
            .border_color(rgb(C_BORDER))
            .text_color(rgb(C_TEXT))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().size(px(9.)).rounded_full().bg(rgb(dot)))
                    .child(div().text_sm().min_w(px(92.)).child(label)),
            )
            .child(
                self.button("device", format!("{target}  ▾"), true)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.picker_open = !this.picker_open;
                        this.refresh_devices(cx);
                        cx.notify();
                    })),
            )
            .child(if idle {
                self.button("connect", "Connect", true)
                    .bg(rgb(C_ACCENT))
                    .on_click(cx.listener(|this, _, _, cx| this.connect(cx)))
            } else {
                self.button("disconnect", "Disconnect", live)
                    .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx)))
            })
            .child(
                div()
                    .id("addr")
                    .track_focus(&self.addr_focus)
                    .on_key_down(cx.listener(|this, ev, window, cx| this.addr_key_down(ev, window, cx)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, _| window.focus(&this.addr_focus)),
                    )
                    .h(px(26.))
                    .w(px(150.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(if addr_focused { C_ACCENT } else { C_BORDER }))
                    .bg(rgb(0x0d1117))
                    .text_sm()
                    .text_color(rgb(if self.addr.is_empty() && !addr_focused { C_MUTED } else { C_TEXT }))
                    .child(addr_text),
            )
            .child(
                self.button("wifi", "Wi-Fi Connect", idle && !self.addr.trim().is_empty())
                    .on_click(cx.listener(|this, _, _, cx| this.connect_wifi(cx))),
            )
            .child(div().flex_1())
            .child(
                self.button("clear", "Clear", true).on_click(cx.listener(|this, _, _, cx| {
                    this.term.clear_all();
                    this.selection = None;
                    if let Some(s) = &this.session {
                        s.send(vec![0x0c]); // Ctrl+L: have the shell redraw its prompt
                    }
                    cx.notify();
                })),
            )
            .child(
                self.button("font-", "A−", true)
                    .on_click(cx.listener(|this, _, _, cx| this.set_font_size(this.font_size - 1.0, cx))),
            )
            .child(div().text_sm().text_color(rgb(C_MUTED)).child(format!("{}px", self.font_size as u32)))
            .child(
                self.button("font+", "A+", true)
                    .on_click(cx.listener(|this, _, _, cx| this.set_font_size(this.font_size + 1.0, cx))),
            )
            .child(
                self.button("cursor", format!("Cursor: {}", self.cursor_shape.label()), true)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.cursor_shape = this.cursor_shape.next();
                        cx.notify();
                    })),
            )
            .child(
                self.button("blink", if self.blink { "Blink: on" } else { "Blink: off" }, true)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.blink = !this.blink;
                        this.blink_on = true;
                        cx.notify();
                    })),
            )
    }

    fn device_picker(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mut list = div()
            .absolute()
            .top(px(48.))
            .left(px(120.))
            .w(px(420.))
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(rgb(C_BORDER))
            .bg(rgb(C_BAR))
            .shadow_lg()
            .text_sm()
            .text_color(rgb(C_TEXT))
            .occlude();
        if self.devices.is_empty() {
            list = list.child(
                div().px_3().py_2().text_color(rgb(C_MUTED)).child("No devices. Plug one in or use Wi-Fi Connect."),
            );
        }
        for (i, d) in self.devices.iter().enumerate() {
            let serial = d.serial.clone();
            let ready = d.is_ready();
            let is_sel = self.selected.as_deref() == Some(d.serial.as_str());
            list = list.child(
                div()
                    .id(("dev", i))
                    .px_3()
                    .py_1()
                    .flex()
                    .justify_between()
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(C_BTN_HOVER)))
                    .when(is_sel, |s| s.bg(rgb(0x1c2a44)))
                    .child(format!("{} · {}", d.display_name(), d.transport.label()))
                    .child(div().text_color(rgb(if ready { C_GREEN } else { C_AMBER })).child(d.state.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = Some(serial.clone());
                        this.picker_open = false;
                        cx.notify();
                    })),
            );
        }
        list
    }

    fn scrollbar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let sb = self.term.scrollback_len();
        let h = f32::from(self.area.size.height);
        let rows = self.term.rows() as f32;
        let total = sb as f32 + rows;
        let thumb_h = (h * rows / total).max(28.0).min(h);
        let frac = if sb == 0 { 1.0 } else { 1.0 - self.term.scroll_offset() as f32 / sb as f32 };
        let thumb_top = (h - thumb_h) * frac;
        let dragging = self.drag == Drag::Scrollbar;
        div()
            .id("scrollbar")
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .w(px(SCROLLBAR_W))
            .bg(rgba(0xffffff08))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.drag = Drag::Scrollbar;
                    this.scrollbar_to(ev.position.y);
                    cx.notify();
                }),
            )
            .when(sb > 0, |s| {
                s.child(
                    div()
                        .absolute()
                        .left(px(2.))
                        .right(px(2.))
                        .top(px(thumb_top))
                        .h(px(thumb_h))
                        .rounded_md()
                        .bg(rgba(if dragging { 0xffffff88 } else { 0xffffff44 })),
                )
            })
    }
}

impl Render for ConsoleView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let bar = self.status_bar(window, cx);
        let offset = self.term.scroll_offset();

        let surface = div()
            .id("terminal")
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .bg(rgb(paint::THEME.bg))
            .track_focus(&self.focus)
            .key_context("Terminal")
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| this.key_down(ev, cx)))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, ev, window, cx| this.mouse_down(ev, window, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _, cx| {
                    // Right click: copy the selection if there is one, otherwise paste.
                    if this.selected_text().is_some() {
                        this.copy_selection(cx);
                        this.selection = None;
                        cx.notify();
                    } else {
                        this.paste(cx);
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| this.mouse_move(ev, cx)))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    this.drag = Drag::None;
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.drag = Drag::None),
            )
            .on_scroll_wheel(cx.listener(|this, ev, _, cx| this.scroll_wheel(ev, cx)))
            .child(
                canvas(
                    move |bounds, window, cx| entity.update(cx, |this, cx| this.prepare(bounds, window, cx)),
                    |_, frame, window, cx| frame.paint(window, cx),
                )
                .absolute()
                .size_full(),
            )
            .child(self.scrollbar(cx))
            .when(offset > 0, |s| {
                s.child(
                    div()
                        .id("live")
                        .absolute()
                        .bottom(px(14.))
                        .right(px(26.))
                        .px_3()
                        .py_1()
                        .rounded_full()
                        .bg(rgb(C_ACCENT))
                        .text_color(rgb(0xffffff))
                        .text_sm()
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.term.scroll_to_bottom();
                            cx.notify();
                        }))
                        .child(format!("↓ Jump to live · {offset} lines back")),
                )
            });

        let mut root = div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(paint::THEME.bg))
            .child(bar)
            .child(surface);
        if let Some(n) = self.notice.clone() {
            root = root.child(
                div()
                    .px_3()
                    .py_1()
                    .bg(rgb(0x3d1a1a))
                    .border_t_1()
                    .border_color(rgb(C_RED))
                    .text_sm()
                    .text_color(rgb(0xffa198))
                    .child(n),
            );
        }
        if self.picker_open {
            root = root.child(self.device_picker(cx));
        }
        root
    }
}
