//! Block selection in the transcript. It holds no ImGui types: the Android layer records the
//! vertical span of each drawn block, feeds touch positions in and puts the copied text on the
//! clipboard.

use std::ops::RangeInclusive;

use crate::model::{Entry, ToolStatus};

/// Lines of a tool's output that are drawn, and so selectable and copied.
const TOOL_OUTPUT_LINES: usize = 6;

/// Which colour a drawn part takes; the Android layer maps it to RGBA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorTag {
    Plain,
    User,
    Dim,
    Output,
    Pending,
    Ok,
    Failed,
    Caution,
    Warning,
}

/// One drawn item of a block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub text: String,
    pub color: ColorTag,
}

/// One selectable unit of the transcript. `parts` are the drawn items; `copy` is the separate
/// text that goes on the clipboard, since decorations and fence lines are drawn but not copied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub entry: usize,
    pub parts: Vec<Part>,
    pub copy: String,
}

fn part(text: impl Into<String>, color: ColorTag) -> Part {
    Part {
        text: text.into(),
        color,
    }
}

fn is_fence(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

fn assistant_blocks(entry: usize, text: &str, out: &mut Vec<Block>) {
    let mut paragraph: Vec<&str> = Vec::new();
    let mut fence_all: Vec<&str> = Vec::new();
    let mut fence_body: Vec<&str> = Vec::new();
    let mut in_fence = false;

    let flush_paragraph = |paragraph: &mut Vec<&str>, out: &mut Vec<Block>| {
        if paragraph.is_empty() {
            return;
        }
        let body = paragraph.join("\n");
        paragraph.clear();
        out.push(Block {
            entry,
            parts: vec![part(body.clone(), ColorTag::Plain)],
            copy: body,
        });
    };
    let flush_fence = |all: &mut Vec<&str>, body: &mut Vec<&str>, out: &mut Vec<Block>| {
        out.push(Block {
            entry,
            parts: vec![part(all.join("\n"), ColorTag::Plain)],
            copy: body.join("\n"),
        });
        all.clear();
        body.clear();
    };

    for line in text.lines() {
        if in_fence {
            fence_all.push(line);
            if is_fence(line) {
                flush_fence(&mut fence_all, &mut fence_body, out);
                in_fence = false;
            } else {
                fence_body.push(line);
            }
        } else if is_fence(line) {
            flush_paragraph(&mut paragraph, out);
            in_fence = true;
            fence_all.push(line);
        } else if line.trim().is_empty() {
            flush_paragraph(&mut paragraph, out);
        } else {
            paragraph.push(line);
        }
    }
    if in_fence {
        flush_fence(&mut fence_all, &mut fence_body, out);
    }
    flush_paragraph(&mut paragraph, out);
}

fn joined(parts: &[Part]) -> String {
    parts
        .iter()
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Splits entries into blocks, in order. Entries that draw nothing yield none, and a reasoning
/// entry yields one only while `open_reasoning` says its text is shown.
pub fn blocks(entries: &[Entry], open_reasoning: impl Fn(usize) -> bool) -> Vec<Block> {
    let mut out = Vec::new();
    for (at, entry) in entries.iter().enumerate() {
        let single = |parts: Vec<Part>, copy: String| Block {
            entry: at,
            parts,
            copy,
        };
        match entry {
            Entry::User(body) => out.push(single(
                vec![part(format!("> {body}"), ColorTag::User)],
                body.clone(),
            )),
            Entry::Error(body) => out.push(single(
                vec![part(format!("! {body}"), ColorTag::Failed)],
                body.clone(),
            )),
            Entry::Notice {
                warning, headline, ..
            } => out.push(single(
                vec![part(
                    headline.clone(),
                    if *warning {
                        ColorTag::Warning
                    } else {
                        ColorTag::Output
                    },
                )],
                headline.clone(),
            )),
            Entry::Attachment(name) => out.push(single(
                vec![part(format!("attached: {name}"), ColorTag::Dim)],
                name.clone(),
            )),
            Entry::Assistant { text, .. } => assistant_blocks(at, text, &mut out),
            Entry::Tool {
                tool,
                summary,
                status,
                output,
            } => {
                let (mark, color) = match status {
                    ToolStatus::Pending => ("...", ColorTag::Pending),
                    ToolStatus::Done => ("ok", ColorTag::Ok),
                    ToolStatus::Failed(_) => ("failed", ColorTag::Failed),
                    ToolStatus::Denied(_) => ("denied", ColorTag::Caution),
                    ToolStatus::Interrupted => ("interrupted", ColorTag::Caution),
                    ToolStatus::Unknown => ("?", ColorTag::Pending),
                };
                let mut parts = vec![part(format!("[{tool}] {summary} - {mark}"), color)];
                if let ToolStatus::Failed(why) | ToolStatus::Denied(why) = status {
                    parts.push(part(why.clone(), ColorTag::Plain));
                }
                if let Some(body) = output {
                    let shown: Vec<&str> = body.lines().take(TOOL_OUTPUT_LINES).collect();
                    if !shown.is_empty() {
                        parts.push(part(shown.join("\n"), ColorTag::Output));
                    }
                }
                let copy = joined(&parts);
                out.push(single(parts, copy));
            }
            Entry::Shell { command, output } => {
                let mut parts = vec![part(format!("$ {command}"), ColorTag::User)];
                if !output.is_empty() {
                    parts.push(part(output.clone(), ColorTag::Output));
                }
                let copy = joined(&parts);
                out.push(single(parts, copy));
            }
            Entry::Reasoning { text, .. } => {
                if open_reasoning(at) {
                    out.push(single(
                        vec![part(text.clone(), ColorTag::Dim)],
                        text.clone(),
                    ));
                }
            }
            Entry::Other(_) => {}
        }
    }
    out
}

/// An inclusive range of block indices; `anchor` is where the press began and `cursor` the end
/// that moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockSelection {
    anchor: usize,
    cursor: usize,
}

impl BlockSelection {
    pub fn new(at: usize) -> Self {
        BlockSelection {
            anchor: at,
            cursor: at,
        }
    }

    pub fn start(&self) -> usize {
        self.anchor.min(self.cursor)
    }

    pub fn end(&self) -> usize {
        self.anchor.max(self.cursor)
    }

    pub fn extend_to(&mut self, at: usize) {
        self.cursor = at;
    }

    /// Moves the start or the end to `at`; dragged past the other end, the ends swap. Returns
    /// whether the dragged handle is the start afterwards, so a drag in progress keeps following
    /// the same finger.
    pub fn move_handle(&mut self, start: bool, at: usize) -> bool {
        let fixed = if start { self.end() } else { self.start() };
        self.anchor = fixed;
        self.cursor = at;
        at <= fixed
    }

    pub fn range(&self) -> RangeInclusive<usize> {
        self.start()..=self.end()
    }

    /// Pulls both ends inside `len` blocks; false when there is nothing left to select.
    pub fn clamp(&mut self, len: usize) -> bool {
        let Some(last) = len.checked_sub(1) else {
            return false;
        };
        self.anchor = self.anchor.min(last);
        self.cursor = self.cursor.min(last);
        true
    }
}

/// The block whose vertical span holds `y`, else the nearest one, so a press in the gap between
/// blocks or a drag past either end still resolves. `None` when there are no blocks.
pub fn block_at(spans: &[(f32, f32)], y: f32) -> Option<usize> {
    let distance = |&(top, bottom): &(f32, f32)| {
        if y < top {
            top - y
        } else if y > bottom {
            y - bottom
        } else {
            0.0
        }
    };
    let mut best: Option<(usize, f32)> = None;
    for (i, span) in spans.iter().enumerate() {
        let d = distance(span);
        if best.is_none_or(|(_, b)| d < b) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

/// The selected blocks' copy text in document order, joined by a blank line.
pub fn copy_text(blocks: &[Block], selection: &BlockSelection) -> String {
    let Some(last) = blocks.len().checked_sub(1) else {
        return String::new();
    };
    let range = selection.start().min(last)..=selection.end().min(last);
    blocks[range]
        .iter()
        .map(|b| b.copy.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(text: &str) -> Entry {
        Entry::Assistant {
            text: text.into(),
            streaming: false,
        }
    }

    fn none_open(_: usize) -> bool {
        false
    }

    #[test]
    fn an_assistant_message_splits_into_paragraphs_and_a_fenced_block() {
        let entries = [assistant(
            "first\nstill first\n\n```rust\nlet a = 1;\n```\n\nlast",
        )];
        let found = blocks(&entries, none_open);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].copy, "first\nstill first");
        assert_eq!(found[1].copy, "let a = 1;");
        assert_eq!(found[1].parts[0].text, "```rust\nlet a = 1;\n```");
        assert_eq!(found[2].copy, "last");
    }

    #[test]
    fn a_blank_line_inside_a_fence_does_not_split_it() {
        let found = blocks(&[assistant("```\na\n\nb\n```")], none_open);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].copy, "a\n\nb");
    }

    #[test]
    fn an_unterminated_fence_is_one_block() {
        let found = blocks(
            &[assistant("intro\n\n```\nfn main() {\n\n    x")],
            none_open,
        );
        assert_eq!(found.len(), 2);
        assert_eq!(found[1].copy, "fn main() {\n\n    x");
    }

    #[test]
    fn entries_that_draw_nothing_yield_no_block() {
        let entries = [Entry::Other("Hologram".into()), assistant("")];
        assert!(blocks(&entries, none_open).is_empty());
    }

    #[test]
    fn reasoning_is_a_block_only_while_open() {
        let entries = [
            Entry::User("hi".into()),
            Entry::Reasoning {
                text: "thinking".into(),
                streaming: false,
            },
        ];
        assert_eq!(blocks(&entries, none_open).len(), 1);
        let open = blocks(&entries, |at| at == 1);
        assert_eq!(open.len(), 2);
        assert_eq!(open[1].entry, 1);
        assert_eq!(open[1].copy, "thinking");
    }

    #[test]
    fn a_tool_block_holds_the_summary_the_reason_and_six_output_lines() {
        let output: Vec<String> = (1..=9).map(|i| format!("line {i}")).collect();
        let entries = [Entry::Tool {
            tool: "bash".into(),
            summary: "ls".into(),
            status: ToolStatus::Failed("exit 1".into()),
            output: Some(output.join("\n")),
        }];
        let found = blocks(&entries, none_open);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].parts.len(), 3);
        assert_eq!(
            found[0].copy,
            "[bash] ls - failed\nexit 1\nline 1\nline 2\nline 3\nline 4\nline 5\nline 6"
        );
    }

    #[test]
    fn a_shell_block_holds_the_command_and_its_output() {
        let entries = [Entry::Shell {
            command: "ls".into(),
            output: "a\n[exit 0]".into(),
        }];
        let found = blocks(&entries, none_open);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].copy, "$ ls\na\n[exit 0]");
    }

    #[test]
    fn what_is_drawn_differs_from_what_is_copied_where_it_is_decorated() {
        let entries = [
            Entry::User("hi".into()),
            Entry::Error("bad".into()),
            Entry::Attachment("notes.txt".into()),
        ];
        let found = blocks(&entries, none_open);
        assert_eq!(found[0].parts[0].text, "> hi");
        assert_eq!(found[0].copy, "hi");
        assert_eq!(found[1].parts[0].text, "! bad");
        assert_eq!(found[1].copy, "bad");
        assert_eq!(found[2].copy, "notes.txt");
    }

    #[test]
    fn copy_text_joins_blocks_with_a_blank_line_in_document_order() {
        let found = blocks(&[assistant("one\n\n```\ncode\n```\n\nthree")], none_open);
        let mut selection = BlockSelection::new(2);
        selection.extend_to(0);
        assert_eq!(copy_text(&found, &selection), "one\n\ncode\n\nthree");
        assert_eq!(copy_text(&found, &BlockSelection::new(1)), "code");
        assert_eq!(copy_text(&[], &BlockSelection::new(0)), "");
    }

    #[test]
    fn extending_and_dragging_a_handle_keep_the_range_ordered() {
        let mut selection = BlockSelection::new(3);
        selection.extend_to(5);
        assert_eq!(selection.range(), 3..=5);
        selection.extend_to(1);
        assert_eq!(selection.range(), 1..=3);

        let mut selection = BlockSelection::new(2);
        selection.extend_to(4);
        assert!(selection.move_handle(true, 3));
        assert_eq!(selection.range(), 3..=4);
        // The start handle dragged past the end becomes the end.
        assert!(!selection.move_handle(true, 6));
        assert_eq!(selection.range(), 4..=6);
        assert!(selection.move_handle(false, 1));
        assert_eq!(selection.range(), 1..=4);
    }

    #[test]
    fn clamp_shrinks_to_the_new_length_and_reports_empty() {
        let mut selection = BlockSelection::new(2);
        selection.extend_to(7);
        assert!(selection.clamp(4));
        assert_eq!(selection.range(), 2..=3);
        assert!(!selection.clamp(0));
    }

    #[test]
    fn block_at_resolves_inside_in_gaps_and_past_the_ends() {
        let spans = [(10.0, 20.0), (30.0, 50.0), (60.0, 70.0)];
        assert_eq!(block_at(&spans, 40.0), Some(1));
        assert_eq!(block_at(&spans, 22.0), Some(0));
        assert_eq!(block_at(&spans, 27.0), Some(1));
        assert_eq!(block_at(&spans, 0.0), Some(0));
        assert_eq!(block_at(&spans, 500.0), Some(2));
        assert_eq!(block_at(&[], 5.0), None);
    }
}
