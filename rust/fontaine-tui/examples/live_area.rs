//! Drives the real `Screen` on a real terminal, with no session behind it: sixty settled notes,
//! then eighty streamed reply lines with tool calls starting and finishing between them. Run it
//! inside tmux to see what the live area does to the terminal's scrollback (see
//! `tasks/design/testing.md`, "Reproducing terminal layout bugs").

use fontaine_core::config::PermissionMode;
use fontaine_core::turn::SessionEvent;
use fontaine_tui::app::App;
use fontaine_tui::screen::{Anchored, Screen};
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::terminal::{
    BeginSynchronizedUpdate, EndSynchronizedUpdate, disable_raw_mode, enable_raw_mode,
};
use std::io::stdout;
use std::time::Duration;

fn main() {
    enable_raw_mode().expect("raw mode");
    let mut screen = Screen::new(Anchored::new(CrosstermBackend::new(stdout())), || {
        Anchored::new(CrosstermBackend::new(stdout()))
    })
    .expect("the terminal answers the cursor query");
    let mut app = App::new(PermissionMode::Manual, "model".into());
    let draw = |app: &mut App, screen: &mut Screen<_>| {
        let _ = ratatui::crossterm::execute!(stdout(), BeginSynchronizedUpdate);
        screen.render(app).expect("render");
        let _ = ratatui::crossterm::execute!(stdout(), EndSynchronizedUpdate);
        std::thread::sleep(Duration::from_millis(15));
    };
    draw(&mut app, &mut screen);
    for i in 0..60 {
        app.transcript.notice(format!("note {i}"));
        draw(&mut app, &mut screen);
    }
    for i in 0..80 {
        if i % 7 == 3 {
            app.on_event(SessionEvent::ToolCall {
                id: format!("t{i}"),
                tool: "shell".into(),
                args: serde_json::json!({ "command": format!("grep -rn foo | head -{i}") }),
            });
        }
        if i % 7 == 5 {
            app.on_event(SessionEvent::ToolResult {
                id: format!("t{}", i - 2),
                tool: "shell".into(),
                success: true,
                text: "done".into(),
            });
        }
        app.on_event(SessionEvent::AssistantDelta(format!(
            "word{i} lorem ipsum dolor sit amet consectetur adipiscing elit sed do\n"
        )));
        draw(&mut app, &mut screen);
    }
    app.flush = true;
    screen.commit_all(&mut app).expect("final commit");
    disable_raw_mode().expect("restore the terminal");
    println!("\nDONE");
}
