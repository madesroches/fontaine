//! The `/rewind` overlay: the prompts of this conversation, newest at the bottom, then what to put
//! back, then for a code restore the files it would touch. It is pure state: the host loads the
//! plan for a prompt the human picks and acts on the result.

use fontaine_core::checkpoint::{PlannedFile, RewindPlan, RewindWhat, TurnSummary};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

/// The most rows the body shows at once.
const MAX_ROWS: usize = 14;

const SHELL_CAVEAT: &str = "changes made through the shell, `!` commands or MCP tools are not tracked and stay as they are";

/// What the human did with the overlay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RewindKey {
    None,
    Close,
    /// The human picked a prompt; the host answers with [`RewindPicker::offer`].
    Select(u64),
    /// The human confirmed an action.
    Run {
        plan: RewindPlan,
        what: RewindWhat,
        overwrite_changed: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    Both,
    Conversation,
    Code,
    NeverMind,
}

const CHOICES: [(Choice, &str); 4] = [
    (Choice::Both, "Restore code and conversation"),
    (Choice::Conversation, "Restore conversation"),
    (Choice::Code, "Restore code"),
    (Choice::NeverMind, "Never mind"),
];

#[derive(Clone, Debug)]
enum Stage {
    Turns,
    Menu {
        plan: RewindPlan,
        selected: usize,
    },
    Confirm {
        plan: RewindPlan,
        what: RewindWhat,
        selected: usize,
    },
}

pub struct RewindPicker {
    turns: Vec<TurnSummary>,
    /// Index into `turns`.
    selected: usize,
    stage: Stage,
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_string()
}

fn cut(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let head: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{head}…")
}

fn step(delta: isize, selected: usize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    (selected as isize + delta).rem_euclid(len as isize) as usize
}

impl RewindPicker {
    /// `turns` are the prompts a rewind may go back to, oldest first.
    pub fn new(turns: Vec<TurnSummary>) -> RewindPicker {
        RewindPicker {
            selected: turns.len().saturating_sub(1),
            turns,
            stage: Stage::Turns,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.turns.is_empty()
    }

    /// The host's answer to [`RewindKey::Select`]: what the prompt's rewind would do.
    pub fn offer(&mut self, plan: RewindPlan) {
        self.stage = Stage::Menu { plan, selected: 0 };
    }

    /// The options of the confirmation: with files changed since, the human chooses whether
    /// they are overwritten.
    fn confirm_options(plan: &RewindPlan) -> Vec<(&'static str, Option<bool>)> {
        if plan.changed_since().next().is_some() {
            vec![
                ("Restore, overwriting the files changed since", Some(true)),
                ("Restore, keeping the files changed since", Some(false)),
                ("Cancel", None),
            ]
        } else {
            vec![("Restore", Some(false)), ("Cancel", None)]
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> RewindKey {
        if key.kind == KeyEventKind::Release {
            return RewindKey::None;
        }
        let delta = match key.code {
            KeyCode::Up => -1,
            KeyCode::Down => 1,
            _ => 0,
        };
        match &mut self.stage {
            Stage::Turns => match key.code {
                KeyCode::Esc => return RewindKey::Close,
                KeyCode::Enter => {
                    if let Some(turn) = self.turns.get(self.selected) {
                        return RewindKey::Select(turn.turn);
                    }
                }
                _ => self.selected = step(delta, self.selected, self.turns.len()),
            },
            Stage::Menu { plan, selected } => match key.code {
                KeyCode::Esc => self.stage = Stage::Turns,
                KeyCode::Enter => {
                    let plan = plan.clone();
                    match CHOICES[*selected].0 {
                        Choice::NeverMind => return RewindKey::Close,
                        Choice::Conversation => {
                            return RewindKey::Run {
                                plan,
                                what: RewindWhat::Conversation,
                                overwrite_changed: false,
                            };
                        }
                        Choice::Both => {
                            self.stage = Stage::Confirm {
                                plan,
                                what: RewindWhat::Both,
                                selected: 0,
                            }
                        }
                        Choice::Code => {
                            self.stage = Stage::Confirm {
                                plan,
                                what: RewindWhat::Code,
                                selected: 0,
                            }
                        }
                    }
                }
                _ => *selected = step(delta, *selected, CHOICES.len()),
            },
            Stage::Confirm {
                plan,
                what,
                selected,
            } => {
                let options = Self::confirm_options(plan);
                match key.code {
                    KeyCode::Esc => {
                        let plan = plan.clone();
                        self.stage = Stage::Menu { plan, selected: 0 };
                    }
                    KeyCode::Enter => match options[*selected].1 {
                        None => return RewindKey::Close,
                        Some(overwrite_changed) => {
                            return RewindKey::Run {
                                plan: plan.clone(),
                                what: *what,
                                overwrite_changed,
                            };
                        }
                    },
                    _ => *selected = step(delta, *selected, options.len()),
                }
            }
        }
        RewindKey::None
    }

    fn row(text: String, chosen: bool, width: usize) -> Line<'static> {
        let style = if chosen {
            Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::default()
        };
        Line::styled(cut(&format!("  {text}"), width), style)
    }

    /// `max_rows` bounds the rows under the header.
    pub fn body_as(&self, width: usize, max_rows: usize) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        match &self.stage {
            Stage::Turns => {
                let mut out = vec![Line::raw(cut("rewind to the start of a prompt", width))];
                if self.turns.is_empty() {
                    out.push(Line::styled("no prompt to rewind to yet", dim));
                    return out;
                }
                let rows: Vec<Line<'static>> = self
                    .turns
                    .iter()
                    .enumerate()
                    .map(|(i, turn)| {
                        let files = match turn.files {
                            0 => "no files".to_string(),
                            1 => "1 file".to_string(),
                            n => format!("{n} files"),
                        };
                        let suffix = format!("  ({files})");
                        let room = width.saturating_sub(suffix.chars().count() + 4).max(10);
                        let prompt = cut(&first_line(&turn.prompt), room);
                        Self::row(format!("{prompt}{suffix}"), i == self.selected, width)
                    })
                    .collect();
                let window = max_rows.clamp(1, MAX_ROWS);
                let start = if rows.len() > window {
                    self.selected
                        .saturating_sub(window / 2)
                        .min(rows.len() - window)
                } else {
                    0
                };
                out.extend(rows.into_iter().skip(start).take(window));
                out
            }
            Stage::Menu { plan, selected } => {
                let mut out = vec![Line::raw(cut(
                    &format!("rewind to: {}", first_line(&plan.prompt)),
                    width,
                ))];
                for (i, (_, label)) in CHOICES.iter().enumerate() {
                    out.push(Self::row((*label).to_string(), i == *selected, width));
                }
                out
            }
            Stage::Confirm {
                plan,
                what,
                selected,
            } => {
                let mut head = vec![Line::raw(cut(
                    &format!("rewind to: {}", first_line(&plan.prompt)),
                    width,
                ))];
                if *what == RewindWhat::Both {
                    head.push(Line::raw("the conversation goes back to that prompt too"));
                }
                head.extend(plan_lines(plan, width));
                let mut tail = vec![Line::styled(cut(SHELL_CAVEAT, width), dim)];
                for (i, (label, _)) in Self::confirm_options(plan).iter().enumerate() {
                    tail.push(Self::row((*label).to_string(), i == *selected, width));
                }
                // The options come last and always show; a long file list gives way to them.
                let room = max_rows.saturating_sub(tail.len()).max(2);
                if head.len() > room {
                    let hidden = head.len() - (room - 1);
                    head.truncate(room - 1);
                    head.push(Line::styled(format!("  … {hidden} more lines"), dim));
                }
                head.extend(tail);
                head
            }
        }
    }

    pub fn footer(&self) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        let text = match self.stage {
            Stage::Turns => "↑/↓ select · Enter choose · Esc cancel",
            Stage::Menu { .. } | Stage::Confirm { .. } => "↑/↓ select · Enter confirm · Esc back",
        };
        vec![Line::styled(text, dim)]
    }
}

fn plan_lines(plan: &RewindPlan, width: usize) -> Vec<Line<'static>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut out = Vec::new();
    let mut group = |title: &str, files: Vec<String>| {
        if files.is_empty() {
            return;
        }
        out.push(Line::raw(cut(
            &format!("{title} ({}):", files.len()),
            width,
        )));
        for file in files {
            out.push(Line::styled(cut(&format!("    {file}"), width), dim));
        }
    };
    let shown = |f: &PlannedFile| f.path.display().to_string();
    group("files restored", plan.to_restore().map(shown).collect());
    group(
        "files deleted (the turn created them)",
        plan.to_delete().map(shown).collect(),
    );
    group(
        "changed since Fontaine last wrote them",
        plan.changed_since().map(shown).collect(),
    );
    group(
        "not restorable",
        plan.not_restorable()
            .map(|(path, why)| format!("{} ({why})", path.display()))
            .collect(),
    );
    if plan.files.is_empty() {
        out.push(Line::styled("no file was changed by tracked edits", dim));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use fontaine_core::checkpoint::{ContentHash, Step};
    use ratatui::crossterm::event::KeyModifiers;

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn turns() -> Vec<TurnSummary> {
        ["first", "second", "third"]
            .iter()
            .enumerate()
            .map(|(i, p)| TurnSummary {
                turn: i as u64 + 1,
                prompt: (*p).to_string(),
                files: i,
            })
            .collect()
    }

    fn plan(turn: u64, changed: bool) -> RewindPlan {
        RewindPlan {
            turn,
            prompt: "second".into(),
            conv_n: 4,
            user_messages_before: 1,
            files: vec![PlannedFile {
                path: PathBuf::from("/w/a.txt"),
                step: Step::Delete,
                changed_since: changed,
            }],
        }
    }

    fn text(picker: &RewindPicker) -> String {
        picker
            .body_as(80, 14)
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_newest_prompt_starts_selected_and_enter_selects_it() {
        let mut picker = RewindPicker::new(turns());
        assert_eq!(picker.handle_key(key(KeyCode::Enter)), RewindKey::Select(3));
        picker.handle_key(key(KeyCode::Up));
        assert_eq!(picker.handle_key(key(KeyCode::Enter)), RewindKey::Select(2));
    }

    #[test]
    fn rows_show_the_prompt_and_its_file_count() {
        let picker = RewindPicker::new(turns());
        let shown = text(&picker);
        assert!(shown.contains("first  (no files)"), "{shown}");
        assert!(shown.contains("second  (1 file)"), "{shown}");
        assert!(shown.contains("third  (2 files)"), "{shown}");
    }

    #[test]
    fn escape_closes_from_the_list_and_steps_back_from_the_menu() {
        let mut picker = RewindPicker::new(turns());
        picker.offer(plan(2, false));
        assert_eq!(picker.handle_key(key(KeyCode::Esc)), RewindKey::None);
        assert_eq!(picker.handle_key(key(KeyCode::Esc)), RewindKey::Close);
    }

    #[test]
    fn a_conversation_restore_runs_without_a_confirmation() {
        let mut picker = RewindPicker::new(turns());
        picker.offer(plan(2, false));
        picker.handle_key(key(KeyCode::Down));
        match picker.handle_key(key(KeyCode::Enter)) {
            RewindKey::Run { what, plan, .. } => {
                assert_eq!(what, RewindWhat::Conversation);
                assert_eq!(plan.turn, 2);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_code_restore_shows_the_plan_first() {
        let mut picker = RewindPicker::new(turns());
        picker.offer(plan(2, false));
        picker.handle_key(key(KeyCode::Down));
        picker.handle_key(key(KeyCode::Down));
        assert_eq!(picker.handle_key(key(KeyCode::Enter)), RewindKey::None);
        let shown = text(&picker);
        assert!(shown.contains("files deleted"), "{shown}");
        assert!(shown.contains("/w/a.txt"), "{shown}");
        assert!(shown.contains("not tracked"), "{shown}");
        assert!(matches!(
            picker.handle_key(key(KeyCode::Enter)),
            RewindKey::Run {
                what: RewindWhat::Code,
                overwrite_changed: false,
                ..
            }
        ));
    }

    #[test]
    fn files_changed_since_need_a_choice_of_whether_to_overwrite() {
        let mut picker = RewindPicker::new(turns());
        picker.offer(plan(2, true));
        picker.handle_key(key(KeyCode::Enter));
        let shown = text(&picker);
        assert!(
            shown.contains("changed since Fontaine last wrote them"),
            "{shown}"
        );
        assert!(matches!(
            picker.handle_key(key(KeyCode::Enter)),
            RewindKey::Run {
                what: RewindWhat::Both,
                overwrite_changed: true,
                ..
            }
        ));
        let mut picker = RewindPicker::new(turns());
        picker.offer(plan(2, true));
        picker.handle_key(key(KeyCode::Enter));
        picker.handle_key(key(KeyCode::Down));
        assert!(matches!(
            picker.handle_key(key(KeyCode::Enter)),
            RewindKey::Run {
                overwrite_changed: false,
                ..
            }
        ));
    }

    #[test]
    fn never_mind_closes() {
        let mut picker = RewindPicker::new(turns());
        picker.offer(plan(2, false));
        for _ in 0..3 {
            picker.handle_key(key(KeyCode::Down));
        }
        assert_eq!(picker.handle_key(key(KeyCode::Enter)), RewindKey::Close);
    }

    #[test]
    fn restore_steps_name_their_files() {
        let hash: ContentHash = serde_json::from_str(&format!("\"{}\"", "a".repeat(64))).unwrap();
        let plan = RewindPlan {
            files: vec![PlannedFile {
                path: PathBuf::from("/w/b.txt"),
                step: Step::Restore {
                    hash,
                    unix_mode: None,
                },
                changed_since: false,
            }],
            ..plan(1, false)
        };
        let lines: Vec<String> = plan_lines(&plan, 80)
            .iter()
            .map(|l| l.to_string())
            .collect();
        assert!(lines[0].starts_with("files restored (1)"), "{lines:?}");
    }
}
