//! Drives the real `Screen` on a real terminal, with no session behind it: sixty settled notes,
//! then eighty streamed reply lines with tool calls (a shell call with fifteen rows of output, an edit with a diff) starting and finishing between them, and an `/expand` at the end. Run it
//! inside tmux to see what the live area does to the terminal's scrollback (see
//! `tasks/design/testing.md`, "Reproducing terminal layout bugs").

use fontaine_core::config::PermissionMode;
use fontaine_core::preview::{DiffKind, DiffLine, DiffPreview};
use fontaine_core::turn::SessionEvent;
use fontaine_tui::app::App;
use fontaine_tui::screen::{Anchored, FrameWriter, Screen, Tty};
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use std::time::Duration;

fn main() {
    enable_raw_mode().expect("raw mode");
    let mut screen = Screen::new(
        Anchored::new(CrosstermBackend::new(FrameWriter::stdout())),
        || Anchored::new(CrosstermBackend::new(FrameWriter::discarding(Tty::std()))),
    )
    .expect("the terminal answers the cursor query");
    let mut app = App::new(PermissionMode::Manual, "model".into());
    let draw = |app: &mut App, screen: &mut Screen<_>| {
        screen.render(app).expect("render");
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
                text: (1..=15).map(|n| format!("output row {n}\n")).collect(),
            });
        }
        if i % 7 == 6 {
            let id = format!("e{i}");
            app.on_event(SessionEvent::ToolCall {
                id: id.clone(),
                tool: "edit".into(),
                args: serde_json::json!({ "path": "src/lib.rs" }),
            });
            app.on_event(SessionEvent::ToolPreview {
                id: id.clone(),
                preview: Box::new(DiffPreview {
                    path: "src/lib.rs".into(),
                    added: 1,
                    removed: 1,
                    lines: vec![
                        DiffLine {
                            kind: DiffKind::Removed,
                            old_no: Some(4),
                            new_no: None,
                            text: "let old = 1;".into(),
                        },
                        DiffLine {
                            kind: DiffKind::Added,
                            old_no: None,
                            new_no: Some(4),
                            text: "let new = 2;".into(),
                        },
                    ],
                    hidden: 0,
                }),
            });
            app.on_event(SessionEvent::ToolResult {
                id,
                tool: "edit".into(),
                success: true,
                text: "edited".into(),
            });
        }
        app.on_event(SessionEvent::AssistantDelta(format!(
            "word{i} lorem ipsum dolor sit amet consectetur adipiscing elit sed do\n"
        )));
        draw(&mut app, &mut screen);
    }
    // The full form of the latest call, printed above the live area like a command would.
    app.transcript.expand(None).expect("a call to expand");
    draw(&mut app, &mut screen);
    app.flush = true;
    screen.commit_all(&mut app).expect("final commit");
    disable_raw_mode().expect("restore the terminal");
    println!("\nDONE");
}
