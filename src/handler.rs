use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_input::backend::crossterm::EventHandler as _;

use crate::app::{App, Focus, Overlay};
use crate::worker::Request;

pub fn handle_key(app: &mut App, key: KeyEvent) {
    if matches!(app.overlay, Overlay::None) {
        handle_global(app, key);
    } else {
        handle_overlay(app, key);
    }
}

fn handle_global(app: &mut App, key: KeyEvent) {
    match (key.code, key.modifiers) {
        (KeyCode::Char('q'), KeyModifiers::NONE) => app.quit(),
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => app.quit(),
        (KeyCode::Tab, _) => app.toggle_focus(),
        (KeyCode::Char('j'), _) | (KeyCode::Down, _) => app.move_selection(1),
        (KeyCode::Char('k'), _) | (KeyCode::Up, _) => app.move_selection(-1),
        (KeyCode::Char('s'), _) => app.wifi.send(Request::Scan),
        (KeyCode::Char('o'), _) => {
            let target = app.state.as_ref().map(|s| !s.powered).unwrap_or(true);
            app.wifi.send(Request::SetPower(target));
        }
        (KeyCode::Char('x'), _) => app.wifi.send(Request::Disconnect),
        (KeyCode::Char('d'), _) => {
            if app.focus == Focus::Preferred {
                if let Some(ssid) = app.selected_preferred() {
                    app.wifi.send(Request::Forget(ssid));
                }
            }
        }
        (KeyCode::Char('p'), _) => app.share_selected_preferred(),
        (KeyCode::Char('h'), _) => app.start_hidden(),
        (KeyCode::Char('i'), _) => app.show_info(),
        (KeyCode::Char('a'), _) => app.toggle_show_all(),
        (KeyCode::Char('A'), _) => app.toggle_show_all_preferred(),
        (KeyCode::Char('T'), _) => app.cycle_theme(1),
        (KeyCode::BackTab, _) => app.cycle_theme(-1),
        (KeyCode::Enter, _) => match app.focus {
            Focus::Available => app.connect_selected_available(),
            Focus::Preferred => app.connect_selected_preferred(),
        },
        _ => {}
    }
}

fn handle_overlay(app: &mut App, key: KeyEvent) {
    if key.code == KeyCode::Esc {
        app.overlay = Overlay::None;
        return;
    }
    if matches!(app.overlay, Overlay::Info | Overlay::Share(_)) {
        // Any key dismisses informational overlays.
        app.overlay = Overlay::None;
        return;
    }
    if key.code == KeyCode::Enter {
        app.submit_overlay();
        return;
    }
    let ct = crossterm::event::Event::Key(key);
    match &mut app.overlay {
        Overlay::Password(p) => {
            p.input.handle_event(&ct);
        }
        Overlay::EnterpriseUser(p) | Overlay::EnterprisePass(p) => {
            p.input.handle_event(&ct);
        }
        Overlay::HiddenSsid(p) | Overlay::HiddenPass(p) => {
            p.input.handle_event(&ct);
        }
        Overlay::Info | Overlay::Share(_) | Overlay::None => {}
    }
}
