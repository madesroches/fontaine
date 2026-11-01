//! Drives the real `Screen` on a real terminal, with no session behind it: sixty settled notes,
//! then eighty streamed reply lines with tool calls (a shell call with fifteen rows of output, an edit with a diff) starting and finishing between them, and an `/expand` at the end. Run it
//! inside tmux to see what the live area does to the terminal's scrollback (see
//! `tasks/design/testing.md`, "Reproducing terminal layout bugs"). `live_area popup` holds the `/`
//! menu open mid-turn so a tmux resize can be tried. `live_area spinner` runs the working
//! indicator through one long tool call, so a resize can be tried while it animates.

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
    if std::env::args().nth(1).as_deref() == Some("popup") {
        popup(&mut app, &mut screen, &draw);
        disable_raw_mode().expect("restore the terminal");
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("spinner") {
        spinner(&mut app, &mut screen, &draw);
        disable_raw_mode().expect("restore the terminal");
        return;
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
    // Markdown arriving in small pieces: a heading, a table whose cells wrap at 50 columns, a
    // list and a fenced block with a blank line in it.
    let markdown = "\n# Markdown check\n\nSome **bold** text with `inline code` in it.\n\n\
| Name | Role | Description |\n|:--|:-:|--:|\n\
| alpha | lead | the first row has a rather long description that must wrap |\n\
| beta | support | a second row |\n\n\
- first item\n- second item with enough words to wrap at the narrower width\n\n\
```rust\nfn main() {\n    println!(\"one\");\n\n    println!(\"two\");\n}\n```\n\nDone.\n";
    let chars: Vec<char> = markdown.chars().collect();
    for piece in chars.chunks(7) {
        app.on_event(SessionEvent::AssistantDelta(piece.iter().collect()));
        draw(&mut app, &mut screen);
    }
    // The full form of the latest call, printed above the live area like a command would.
    app.transcript
        .expand(fontaine_tui::commands::ExpandTarget::Latest)
        .expect("a call to expand");
    draw(&mut app, &mut screen);
    app.flush = true;
    screen.commit_all(&mut app).expect("final commit");
    disable_raw_mode().expect("restore the terminal");
    println!("\nDONE");
}

/// Issue 279: the `/` menu typed open during a running turn, with a task list, a finished shell
/// call with output, two extra notices and the next call starting under the open menu. Leaves
/// the live area on screen and holds it, so tmux can be captured.
fn popup<S, D>(app: &mut App, screen: &mut S, draw: &D)
where
    D: Fn(&mut App, &mut S),
{
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let key = |app: &mut App, code| {
        app.handle_input(fontaine_tui::Input::Key(KeyEvent::new(
            code,
            KeyModifiers::NONE,
        )))
    };
    app.running = true;
    app.todos.set_items(
        (0..7)
            .map(|n| fontaine_tools::todo::TodoItem {
                content: format!("task {n}"),
                status: fontaine_tools::todo::TodoStatus::Completed,
            })
            .collect(),
    );
    app.on_event(SessionEvent::ToolCall {
        id: "a".into(),
        tool: "shell".into(),
        args: serde_json::json!({ "command": "cargo fmt --check && cargo test; echo GATE_DONE" }),
    });
    draw(app, screen);
    for c in "/guardian-en".chars() {
        key(app, KeyCode::Char(c));
        draw(app, screen);
    }
    app.on_event(SessionEvent::ToolResult {
        id: "a".into(),
        tool: "shell".into(),
        success: true,
        text: "    Finished `dev` profile\nGATE_DONE\n[exit code 0]".into(),
    });
    draw(app, screen);
    for c in "able".chars() {
        key(app, KeyCode::Char(c));
        draw(app, screen);
    }
    for _ in 0..2 {
        app.transcript
            .notice("a turn is running; wait for it or press Esc to interrupt");
        draw(app, screen);
    }
    app.on_event(SessionEvent::ToolCall {
        id: "b".into(),
        tool: "shell".into(),
        args: serde_json::json!({ "command": "git add -A && git commit -m \"plugins: load manifest component paths\" && git push && git log --oneline -2" }),
    });
    // Keeps drawing, as the app does on its ticks and on resize, so a resize can be tried.
    for _ in 0..4000 {
        draw(app, screen);
    }
}

/// The working indicator across a long shell call: the row appears, animates for about twenty
/// seconds (long enough to resize the window), then goes when the turn ends.
fn spinner<S, D>(app: &mut App, screen: &mut S, draw: &D)
where
    D: Fn(&mut App, &mut S),
{
    app.set_running(true);
    app.on_event(SessionEvent::ToolCall {
        id: "a".into(),
        tool: "shell".into(),
        args: serde_json::json!({ "command": "sleep 20; echo SPIN_DONE" }),
    });
    draw(app, screen);
    for frame in 0..250 {
        app.spinner_frame = frame;
        app.turn_elapsed = Some(Duration::from_millis(frame * 80));
        draw(app, screen);
        std::thread::sleep(Duration::from_millis(65));
    }
    app.on_event(SessionEvent::ToolResult {
        id: "a".into(),
        tool: "shell".into(),
        success: true,
        text: "SPIN_DONE\n[exit code 0]".into(),
    });
    app.set_running(false);
    draw(app, screen);
}
