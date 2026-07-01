use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Padding, Paragraph, Row, Table, TableState,
};

use crate::app::{App, Focus, Overlay};
use crate::corewlan::{InterfaceState, Security};
use crate::event::SharePayload;
use crate::notification::Notification;
use crate::theme::Theme;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();

    // Layout mirrors impala's station mode: known networks on top, new
    // networks below, the device table near the bottom, then the help line.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),
            Constraint::Min(5),
            Constraint::Length(5),
            Constraint::Length(2),
        ])
        .margin(1)
        .split(area);

    draw_known_networks(f, chunks[0], app);
    draw_new_networks(f, chunks[1], app);
    draw_device(f, chunks[2], app);
    draw_help(f, chunks[3], app.focus, app.theme);
    draw_notifications(f, area, &app.notifications, app.theme);

    let theme = app.theme;
    match &app.overlay {
        Overlay::None => {}
        Overlay::Password(p) => draw_input(f, area, theme, "Password", &p.ssid, p.input.value(), true),
        Overlay::EnterpriseUser(p) => {
            draw_input(f, area, theme, "Username (PEAP)", &p.ssid, p.input.value(), false)
        }
        Overlay::EnterprisePass(p) => {
            draw_input(f, area, theme, "Password (PEAP)", &p.ssid, p.input.value(), true)
        }
        Overlay::HiddenSsid(p) => {
            draw_input(f, area, theme, "Hidden SSID", "", p.input.value(), false)
        }
        Overlay::HiddenPass(p) => {
            draw_input(f, area, theme, "Hidden password (blank = open)", &p.ssid, p.input.value(), true)
        }
        Overlay::Info => draw_info(f, area, theme, app.state.as_ref()),
        Overlay::Share(payload) => draw_share(f, area, theme, payload),
    }
}

//
// Known networks (macOS preferred networks) — impala's "Known Networks" table.
//
fn draw_known_networks(f: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Preferred;
    let theme = app.theme;
    let connected = app.state.as_ref().and_then(|s| s.ssid.clone());

    let visible = app.visible_preferred();
    let rows: Vec<Row> = visible
        .iter()
        .map(|ssid| {
            let net = app.networks.iter().find(|n| n.ssid.as_deref() == Some(ssid));
            let icon = if connected.as_deref() == Some(ssid) {
                "󰖩 "
            } else {
                ""
            };
            // impala parity: out-of-range known networks (no scan match, only
            // shown via the `A` toggle) render dimmed with blank detail columns.
            match net {
                Some(n) => Row::new(vec![
                    Line::from(icon).centered(),
                    Line::from(ssid.clone()).centered(),
                    Line::from(sec_label(n.security).to_string()).centered(),
                    Line::from("No").centered(),
                    Line::from("Yes").centered(),
                    Line::from(format!("{}%", signal_pct(n.rssi))).centered(),
                ]),
                None => Row::new(vec![
                    Line::from(icon).centered(),
                    Line::from(ssid.clone()).centered(),
                    Line::from("").centered(),
                    Line::from("").centered(),
                    Line::from("").centered(),
                    Line::from("").centered(),
                ])
                .dark_gray(),
            }
        })
        .collect();

    let widths = [
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(8),
        Constraint::Length(6),
        Constraint::Length(12),
        Constraint::Length(6),
    ];

    let header = if focused {
        Row::new(vec![
            Line::from(""),
            Line::from("Name").centered(),
            Line::from("Security").centered(),
            Line::from("Hidden").centered(),
            Line::from("Auto Connect").centered(),
            Line::from("Signal").centered(),
        ])
        .style(Style::new().fg(theme.accent).bold())
        .bottom_margin(1)
    } else {
        Row::new(vec![
            Line::from(""),
            Line::from("Name").centered(),
            Line::from("Security").centered(),
            Line::from("Hidden").centered(),
            Line::from("Auto Connect").centered(),
            Line::from("Signal").centered(),
        ])
        .bottom_margin(1)
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(block(" Known Networks ", focused, theme))
        .column_spacing(1)
        .flex(Flex::SpaceAround)
        .row_highlight_style(highlight(focused, theme));

    f.render_stateful_widget(table, area, &mut app.preferred_state);
}

//
// New networks (scan results) — impala's "New Networks" table.
//
fn draw_new_networks(f: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Available;
    let theme = app.theme;

    let visible = app.visible_networks();
    let rows: Vec<Row> = visible
        .iter()
        .map(|n| {
            let name = n.ssid.clone().unwrap_or_else(|| "<hidden>".into());
            let pct = signal_pct(n.rssi);
            let signal = format!("{:3}% {}", pct, signal_icon(pct));
            Row::new(vec![
                Line::from(name).centered(),
                Line::from(sec_label(n.security).to_string()).centered(),
                Line::from(signal).centered(),
            ])
        })
        .collect();

    let widths = [
        Constraint::Fill(1),
        Constraint::Length(15),
        Constraint::Length(8),
    ];

    let header = if focused {
        Row::new(vec![
            Line::from("Name").centered(),
            Line::from("Security").centered(),
            Line::from("Signal").centered(),
        ])
        .style(Style::new().fg(theme.accent).bold())
        .bottom_margin(1)
    } else {
        Row::new(vec![
            Line::from("Name").centered(),
            Line::from("Security").centered(),
            Line::from("Signal").centered(),
        ])
        .bottom_margin(1)
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(block(" New Networks ", focused, theme))
        .column_spacing(1)
        .flex(Flex::SpaceAround)
        .row_highlight_style(highlight(focused, theme));

    f.render_stateful_widget(table, area, &mut app.available_state);
}

//
// Device — impala's "Device" table.
//
fn draw_device(f: &mut Frame, area: Rect, app: &App) {
    let row = match &app.state {
        Some(s) => {
            let security = s
                .ssid
                .as_deref()
                .and_then(|ssid| app.networks.iter().find(|n| n.ssid.as_deref() == Some(ssid)))
                .map(|n| sec_label(n.security).to_string())
                .unwrap_or_else(|| "-".into());
            Row::new(vec![
                Line::from(s.name.clone()).centered(),
                Line::from("station").centered(),
                Line::from(if s.powered { "On" } else { "Off" }).centered(),
                Line::from(if s.ssid.is_some() { "connected" } else { "disconnected" }).centered(),
                Line::from(if app.scanning { "Yes" } else { "No" }).centered(),
                Line::from(band(s.channel)).centered(),
                Line::from(security).centered(),
            ])
        }
        None => Row::new(vec![
            Line::from("-").centered(),
            Line::from("station").centered(),
            Line::from("-").centered(),
            Line::from("-").centered(),
            Line::from(if app.scanning { "Yes" } else { "No" }).centered(),
            Line::from("-").centered(),
            Line::from("-").centered(),
        ]),
    };

    let widths = [
        Constraint::Length(10),
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(12),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(15),
    ];

    let header = Row::new(vec![
        Line::from("Name").centered(),
        Line::from("Mode").centered(),
        Line::from("Powered").centered(),
        Line::from("State").centered(),
        Line::from("Scanning").centered(),
        Line::from("Frequency").centered(),
        Line::from("Security").centered(),
    ])
    .bottom_margin(1);

    let table = Table::new(vec![row], widths)
        .header(header)
        .block(block(" Device ", false, app.theme))
        .column_spacing(1)
        .flex(Flex::SpaceAround);

    let mut state = TableState::default().with_selected(0);
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_help(f: &mut Frame, area: Rect, focus: Focus, theme: Theme) {
    let lines = match focus {
        Focus::Preferred => vec![
            Line::from(vec![
                Span::from("k").bold(),
                Span::from(" Up"),
                Span::from(" | "),
                Span::from("j").bold(),
                Span::from(" Down"),
                Span::from(" | "),
                Span::from("↵").bold(),
                Span::from(" Connect"),
                Span::from(" | "),
                Span::from("d").bold(),
                Span::from(" Remove"),
                Span::from(" | "),
                Span::from("p").bold(),
                Span::from(" Share"),
                Span::from(" | "),
                Span::from("A").bold(),
                Span::from(" Show All"),
                Span::from(" | "),
                Span::from("tab").bold(),
                Span::from(" Nav"),
            ]),
            Line::from(vec![
                Span::from("s").bold(),
                Span::from(" Scan"),
                Span::from(" | "),
                Span::from("o").bold(),
                Span::from(" Power"),
                Span::from(" | "),
                Span::from("x").bold(),
                Span::from(" Disconnect"),
                Span::from(" | "),
                Span::from("i").bold(),
                Span::from(" Infos"),
                Span::from(" | "),
                Span::from("T").bold(),
                Span::from(" Theme"),
                Span::from(" | "),
                Span::from("q").bold(),
                Span::from(" Quit"),
            ]),
        ],
        Focus::Available => vec![
            Line::from(vec![
                Span::from("k").bold(),
                Span::from(" Up"),
                Span::from(" | "),
                Span::from("j").bold(),
                Span::from(" Down"),
                Span::from(" | "),
                Span::from("↵").bold(),
                Span::from(" Connect"),
                Span::from(" | "),
                Span::from("h").bold(),
                Span::from(" Connect Hidden"),
                Span::from(" | "),
                Span::from("a").bold(),
                Span::from(" Show All"),
                Span::from(" | "),
                Span::from("tab").bold(),
                Span::from(" Nav"),
            ]),
            Line::from(vec![
                Span::from("s").bold(),
                Span::from(" Scan"),
                Span::from(" | "),
                Span::from("o").bold(),
                Span::from(" Power"),
                Span::from(" | "),
                Span::from("x").bold(),
                Span::from(" Disconnect"),
                Span::from(" | "),
                Span::from("i").bold(),
                Span::from(" Infos"),
                Span::from(" | "),
                Span::from("T").bold(),
                Span::from(" Theme"),
                Span::from(" | "),
                Span::from("q").bold(),
                Span::from(" Quit"),
            ]),
        ],
    };
    f.render_widget(
        Paragraph::new(lines).centered().fg(theme.accent),
        area,
    );
}

//
// Shared impala-style block / styling helpers, tinted by the active theme.
//
fn block(title: &'static str, focused: bool, theme: Theme) -> Block<'static> {
    Block::default()
        .title(title)
        .title_style(if focused {
            Style::default().fg(theme.title).bold()
        } else {
            Style::default().fg(theme.title)
        })
        .borders(Borders::ALL)
        .border_style(if focused {
            Style::default().fg(theme.border_focused)
        } else {
            Style::default().fg(theme.border)
        })
        .border_type(if focused {
            BorderType::Thick
        } else {
            BorderType::default()
        })
        .padding(Padding::horizontal(1))
}

fn highlight(focused: bool, theme: Theme) -> Style {
    if focused {
        Style::default().bg(theme.accent).fg(theme.accent_fg)
    } else {
        Style::default()
    }
}

fn signal_pct(dbm: isize) -> i64 {
    if dbm >= -50 {
        100
    } else {
        (2 * (100 + dbm as i64)).max(0)
    }
}

fn signal_icon(pct: i64) -> char {
    match pct {
        n if n >= 75 => '󰤨',
        n if (50..75).contains(&n) => '󰤥',
        n if (25..50).contains(&n) => '󰤢',
        _ => '󰤟',
    }
}

fn band(channel: Option<u32>) -> String {
    match channel {
        Some(c) if (1..=14).contains(&c) => "2.4 GHz".into(),
        Some(_) => "5 GHz".into(),
        None => "-".into(),
    }
}

//
// Overlays / notifications (functional macwifi pieces, themed).
//
fn draw_notifications(f: &mut Frame, area: Rect, ns: &[Notification], theme: Theme) {
    if ns.is_empty() {
        return;
    }
    let visible: Vec<_> = ns.iter().rev().take(5).collect();
    let h = visible.len() as u16 + 2;
    let w = 60.min(area.width.saturating_sub(2));
    let x = area.x + area.width.saturating_sub(w + 1);
    let y = area.y + area.height.saturating_sub(h + 2);
    let rect = Rect::new(x, y, w, h);
    f.render_widget(Clear, rect);
    let lines: Vec<Line> = visible
        .into_iter()
        .map(|n| {
            let color = match n.kind {
                crate::notification::Kind::Info => theme.ok,
                crate::notification::Kind::Error => theme.err,
            };
            Line::from(Span::styled(n.text.clone(), style_fg(color)))
        })
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" notifications ", style_fg(theme.title)))
        .border_style(style_fg(theme.border))
        .style(Style::default().bg(theme.surface).fg(theme.fg));
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

fn draw_input(f: &mut Frame, area: Rect, theme: Theme, label: &str, ssid: &str, value: &str, mask: bool) {
    let w = 60.min(area.width.saturating_sub(4));
    let h = 5;
    let rect = centered(area, w, h);
    f.render_widget(Clear, rect);
    let title = if ssid.is_empty() {
        format!(" {label} ")
    } else {
        format!(" {label} — {ssid} ")
    };
    let shown = if mask {
        "•".repeat(value.chars().count())
    } else {
        value.to_string()
    };
    let body = vec![
        Line::from(Span::styled(format!("> {shown}"), style_fg(theme.fg))),
        Line::from(Span::styled(
            "Enter submit  •  Esc cancel",
            style_fg(theme.muted),
        )),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(title, style_fg(theme.title)))
        .border_style(style_fg(theme.border_focused))
        .style(Style::default().bg(theme.surface).fg(theme.fg))
        .title_alignment(Alignment::Left);
    f.render_widget(Paragraph::new(body).block(block), rect);
}

fn draw_info(f: &mut Frame, area: Rect, theme: Theme, state: Option<&InterfaceState>) {
    let w = 60.min(area.width.saturating_sub(4));
    let h = 12;
    let rect = centered(area, w, h);
    f.render_widget(Clear, rect);
    let lines: Vec<Line> = match state {
        Some(s) => vec![
            kv("interface", s.name.clone(), theme),
            kv("powered", if s.powered { "ON".into() } else { "OFF".into() }, theme),
            kv("hw addr", s.hw_address.clone().unwrap_or_else(|| "—".into()), theme),
            kv("SSID", s.ssid.clone().unwrap_or_else(|| "—".into()), theme),
            kv("BSSID", s.bssid.clone().unwrap_or_else(|| "—".into()), theme),
            kv("RSSI", format!("{} dBm", s.rssi), theme),
            kv("noise", format!("{} dBm", s.noise), theme),
            kv("tx rate", format!("{} Mbps", s.tx_rate), theme),
            kv(
                "channel",
                s.channel
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "—".into()),
                theme,
            ),
        ],
        None => vec![Line::from(Span::raw("(no state)"))],
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" Adapter info ", style_fg(theme.title)))
        .border_style(style_fg(theme.border_focused))
        .style(Style::default().bg(theme.surface).fg(theme.fg));
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

fn kv(k: &str, v: String, theme: Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {:<10} ", k), style_fg(theme.muted)),
        Span::styled(v, Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)),
    ])
}

fn draw_share(f: &mut Frame, area: Rect, theme: Theme, p: &SharePayload) {
    let code = qrcode::QrCode::new(p.uri.as_bytes());
    let body_lines: Vec<Line> = match code {
        Ok(c) => {
            let s = c
                .render::<qrcode::render::unicode::Dense1x2>()
                .quiet_zone(true)
                .dark_color(qrcode::render::unicode::Dense1x2::Dark)
                .light_color(qrcode::render::unicode::Dense1x2::Light)
                .build();
            s.lines()
                .map(|l| Line::from(Span::styled(l.to_string(), style_fg(theme.fg))))
                .collect()
        }
        Err(e) => vec![Line::from(Span::styled(
            format!("QR error: {e}"),
            style_fg(theme.err),
        ))],
    };
    let qr_h = body_lines.len() as u16;
    let qr_w = body_lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.chars().count()).sum::<usize>())
        .max()
        .unwrap_or(0) as u16;
    let w = (qr_w + 4).max(40).min(area.width.saturating_sub(2));
    let h = (qr_h + 4).max(8).min(area.height.saturating_sub(2));
    let rect = centered(area, w, h);
    f.render_widget(Clear, rect);

    let mut lines = body_lines;
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        if p.has_password {
            "scan with phone camera to join · Esc close"
        } else {
            "SSID-only QR (no password) · Esc close"
        },
        style_fg(theme.muted),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" Share — {} ", p.ssid),
            style_fg(theme.title),
        ))
        .border_style(style_fg(theme.border_focused))
        .style(Style::default().bg(theme.surface).fg(theme.fg));
    f.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .block(block),
        rect,
    );
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w.min(area.width), h.min(area.height))
}

fn style_fg(c: Color) -> Style {
    Style::default().fg(c)
}

fn sec_label(s: Security) -> &'static str {
    match s {
        Security::Open => "open",
        Security::Wep => "WEP",
        Security::WpaPersonal => "WPA",
        Security::Wpa2Personal => "WPA2",
        Security::Wpa3Personal => "WPA3",
        Security::WpaEnterprise => "WPA-E",
        Security::Wpa2Enterprise => "WPA2-E",
        Security::Wpa3Enterprise => "WPA3-E",
        Security::Unknown => "?",
    }
}
