use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};

use crate::app::{App, Focus, Overlay};
use crate::corewlan::{InterfaceState, Security};
use crate::event::SharePayload;
use crate::notification::Notification;
use crate::theme::Theme;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let theme = app.theme;
    paint_background(f, area, theme);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(8),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .split(area);

    draw_status(f, chunks[0], app);
    draw_preferred(f, chunks[1], app);
    draw_available(f, chunks[2], app);
    draw_help(f, chunks[3], theme);
    draw_notifications(f, area, &app.notifications, theme);

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

fn paint_background(f: &mut Frame, area: Rect, theme: Theme) {
    let block = Block::default().style(Style::default().bg(theme.bg).fg(theme.fg));
    f.render_widget(block, area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let mut spans = Vec::new();
    if let Some(s) = &app.state {
        let (label, color) = if s.powered {
            ("ON", theme.ok)
        } else {
            ("OFF", theme.err)
        };
        spans.push(Span::styled(
            format!(" {} ", s.name),
            Style::default().fg(theme.title).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled("│ ", style_fg(theme.muted)));
        spans.push(Span::styled(
            label,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(" │ ", style_fg(theme.muted)));
        spans.push(Span::raw("SSID: "));
        spans.push(Span::styled(
            s.ssid.clone().unwrap_or_else(|| "—".into()),
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(format!("  RSSI: {} dBm", s.rssi)));
        if let Some(c) = s.channel {
            spans.push(Span::raw(format!("  CH: {c}")));
        }
        spans.push(Span::raw(format!("  TX: {:.0} Mbps", s.tx_rate)));
        if app.scanning {
            spans.push(Span::styled("  ⟳ scanning", style_fg(theme.warn)));
        }
    } else {
        spans.push(Span::raw(" (no interface state yet) "));
    }
    let p = Paragraph::new(Line::from(spans)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(style_fg(theme.border))
            .title(Span::styled(" macwifi ", style_fg(theme.title).add_modifier(Modifier::BOLD)))
            .style(Style::default().bg(theme.bg).fg(theme.fg)),
    );
    f.render_widget(p, area);
}

fn draw_preferred(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = app.theme;
    let items: Vec<ListItem> = app
        .preferred
        .iter()
        .map(|s| ListItem::new(s.clone()))
        .collect();
    let focused = app.focus == Focus::Preferred;
    let title = format!(" Preferred ({}) ", app.preferred.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(title, style_fg(theme.title)))
        .border_style(border_style(theme, focused))
        .style(Style::default().bg(theme.bg).fg(theme.fg));
    let list = List::new(items)
        .block(block)
        .highlight_style(highlight_style(theme, focused))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, &mut app.preferred_state);
}

fn draw_available(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = app.theme;
    let visible = app.visible_networks();
    let items: Vec<ListItem> = visible
        .iter()
        .map(|n| {
            let ssid = n.ssid.clone().unwrap_or_else(|| "<hidden>".into());
            let line = format!(
                "{:<32}  {:>4} dBm  ch{:<3}  {}",
                truncate(&ssid, 32),
                n.rssi,
                n.channel.map(|c| c.to_string()).unwrap_or_else(|| "?".into()),
                sec_label(n.security),
            );
            ListItem::new(line)
        })
        .collect();
    let focused = app.focus == Focus::Available;
    let title = format!(
        " Available ({}/{}){}{}",
        visible.len(),
        app.networks.len(),
        if app.scanning { " — scanning" } else { "" },
        if app.show_all { " — all " } else { " " },
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(title, style_fg(theme.title)))
        .border_style(border_style(theme, focused))
        .style(Style::default().bg(theme.bg).fg(theme.fg));
    let list = List::new(items)
        .block(block)
        .highlight_style(highlight_style(theme, focused))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, &mut app.available_state);
}

fn draw_help(f: &mut Frame, area: Rect, theme: Theme) {
    let text =
        " Tab focus │ j/k │ Enter connect │ s scan │ o power │ d forget │ x off │ p share │ h hidden │ i info │ a all │ T theme │ q quit ";
    f.render_widget(
        Paragraph::new(text).style(Style::default().bg(theme.bg).fg(theme.muted)),
        area,
    );
}

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

fn border_style(theme: Theme, focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(theme.border_focused)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.border)
    }
}

fn highlight_style(theme: Theme, focused: bool) -> Style {
    if focused {
        Style::default()
            .bg(theme.accent)
            .fg(theme.accent_fg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::REVERSED)
    }
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

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
