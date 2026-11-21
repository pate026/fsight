//! Key handling: turns a key press into a state transition on [`App`].
//!
//! This is a plain function of `(App, KeyEvent)`, so tests can feed it
//! hand-made key events without a terminal.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::app::{App, Mode};

/// Applies one key press to the app.
pub fn handle_key(app: &mut App, key: KeyEvent) {
    // Some terminals also report key releases and repeats; act on presses only.
    if key.kind == KeyEventKind::Release {
        return;
    }
    // Ctrl-C always quits, whatever mode we are in.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.quit();
        return;
    }
    // Any key dismisses the previous one-off message.
    app.status = None;
    match app.mode {
        Mode::Help => help_key(app, key.code),
        Mode::Filter => filter_key(app, key.code),
        Mode::Normal => normal_key(app, key.code),
    }
}

fn normal_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Up | KeyCode::Char('k') => app.move_by(-1),
        KeyCode::Down | KeyCode::Char('j') => app.move_by(1),
        KeyCode::PageUp => app.page_by(-1),
        KeyCode::PageDown => app.page_by(1),
        KeyCode::Home | KeyCode::Char('g') => app.move_to_top(),
        KeyCode::End | KeyCode::Char('G') => app.move_to_bottom(),
        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => app.open_selected(),
        KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => app.go_up(),
        KeyCode::Char('s') => app.cycle_sort(),
        KeyCode::Char('/') => app.start_filter(),
        KeyCode::Esc => app.clear_filter(),
        KeyCode::Char('i') => app.toggle_info(),
        KeyCode::Char('?') => app.toggle_help(),
        KeyCode::Char('q') => app.quit(),
        _ => {}
    }
}

/// While typing a filter, letters are text — `j` must not move the cursor.
fn filter_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char(c) => app.push_filter(c),
        KeyCode::Backspace => app.pop_filter(),
        KeyCode::Enter => app.finish_filter(),
        KeyCode::Esc => app.clear_filter(),
        KeyCode::Up => app.move_by(-1),
        KeyCode::Down => app.move_by(1),
        _ => {}
    }
}

fn help_key(app: &mut App, code: KeyCode) {
    if matches!(
        code,
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?' | 'q')
    ) {
        app.toggle_help();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::tests::{names, ready_app};

    fn press(app: &mut App, code: KeyCode) {
        handle_key(app, KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn type_text(app: &mut App, text: &str) {
        text.chars().for_each(|c| press(app, KeyCode::Char(c)));
    }

    #[test]
    fn vim_keys_and_arrows_both_navigate() {
        let mut app = ready_app();
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Down);
        assert_eq!(app.selected, 2);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.selected, 1);
        press(&mut app, KeyCode::Char('l')); // open "mid"
        assert_eq!(names(&app), ["c"]);
        press(&mut app, KeyCode::Left);
        assert_eq!(names(&app), ["big", "mid", "Small.txt"]);
        press(&mut app, KeyCode::Enter); // cursor is back on "mid"
        assert_eq!(names(&app), ["c"]);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Char('h')); // already at the root
        assert!(app.status.is_some());
    }

    #[test]
    fn typing_in_filter_mode_edits_the_filter_instead_of_navigating() {
        let mut app = ready_app();
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "jk");
        assert_eq!(app.filter, "jk");
        assert_eq!(app.selected, 0);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        type_text(&mut app, "mi");
        assert_eq!(names(&app), ["mid"]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.filter, "mi", "Enter keeps the filter");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.filter, "", "Esc clears it");
    }

    #[test]
    fn help_popup_swallows_keys_until_closed() {
        let mut app = ready_app();
        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.mode, Mode::Help);
        press(&mut app, KeyCode::Char('j')); // ignored while help is open
        assert_eq!(app.selected, 0);
        press(&mut app, KeyCode::Char('q')); // closes help, does not quit
        assert_eq!(app.mode, Mode::Normal);
        assert!(!app.should_quit);
    }

    #[test]
    fn q_and_ctrl_c_quit() {
        let mut app = ready_app();
        press(&mut app, KeyCode::Char('q'));
        assert!(app.should_quit);

        let mut app = ready_app();
        press(&mut app, KeyCode::Char('/'));
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        assert!(app.should_quit, "Ctrl-C quits even while typing a filter");
    }

    #[test]
    fn sort_info_and_key_releases() {
        let mut app = ready_app();
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.sort.label(), "name");
        press(&mut app, KeyCode::Char('i'));
        assert!(app.show_info);
        let mut release = KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        handle_key(&mut app, release);
        assert!(app.show_info, "releases are ignored");
    }
}
