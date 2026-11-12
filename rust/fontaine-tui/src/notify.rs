//! Tells the terminal that a prompt is waiting for the human, so a user who switched windows
//! finds out.

use fontaine_core::config::NotifyMethod;

/// The prompts that wait for an answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PromptKind {
    Approval,
    Question,
    Elicitation,
    Server,
    GuardianOff,
    Bug,
    AfkReview,
    Unavailable,
    UsagePause,
    Skills,
    Plugin,
    SetupReview,
    SetupComposition,
}

impl PromptKind {
    /// The `notification` hook's type for the prompt, for the kinds that fire it: an approval
    /// is a `permission_prompt`, a question an `elicitation_dialog`.
    pub(crate) fn hook_type(self) -> Option<&'static str> {
        match self {
            PromptKind::Approval => Some("permission_prompt"),
            PromptKind::Question => Some("elicitation_dialog"),
            _ => None,
        }
    }

    /// What the notification says. Fixed text: nothing from the model or a tool reaches the
    /// escape sequence.
    pub(crate) fn message(self) -> &'static str {
        match self {
            PromptKind::Approval => "Fontaine needs your approval",
            PromptKind::Question => "Fontaine has a question",
            PromptKind::Elicitation => "An MCP server asks for input",
            PromptKind::Server => "Fontaine needs an MCP server decision",
            PromptKind::GuardianOff | PromptKind::AfkReview | PromptKind::Bug => {
                "Fontaine needs your confirmation"
            }
            PromptKind::Unavailable => "The guardian is unavailable",
            PromptKind::UsagePause => "A usage window is spent and the run is paused",
            PromptKind::Skills => "Fontaine needs a skill trust decision",
            PromptKind::Plugin => "Fontaine needs a plugin approval",
            PromptKind::SetupReview | PromptKind::SetupComposition => {
                "Auto-mode setup needs your review"
            }
        }
    }
}

/// Signals once per prompt opening.
pub(crate) struct Notifier {
    method: NotifyMethod,
    /// Every prompt signalled while one has been pending, so an answered higher-ranked prompt
    /// does not re-signal the prompt it covered.
    seen: Vec<(PromptKind, u64)>,
}

impl Notifier {
    pub(crate) fn new(method: NotifyMethod) -> Self {
        Notifier {
            method,
            seen: Vec::new(),
        }
    }

    /// Bytes to write with this frame, if a prompt that needs an answer just opened. A key not
    /// seen yet signals; `None` forgets them all, so the next prompt signals even when it is of
    /// the same kind.
    pub(crate) fn observe(&mut self, pending: Option<(PromptKind, u64)>) -> Option<Vec<u8>> {
        self.observe_opened(pending).1
    }

    /// Like [`observe`](Self::observe), and also the prompt that just opened, whatever the
    /// method: the `notification` hook fires on the opening itself.
    pub(crate) fn observe_opened(
        &mut self,
        pending: Option<(PromptKind, u64)>,
    ) -> (Option<PromptKind>, Option<Vec<u8>>) {
        let Some(key) = pending else {
            self.seen.clear();
            return (None, None);
        };
        if self.seen.contains(&key) {
            return (None, None);
        }
        self.seen.push(key);
        let (kind, _) = key;
        (Some(kind), self.bytes_for(kind))
    }

    fn bytes_for(&self, kind: PromptKind) -> Option<Vec<u8>> {
        let text = kind.message();
        match self.method {
            NotifyMethod::Off => None,
            NotifyMethod::Bell => Some(b"\x07".to_vec()),
            NotifyMethod::Osc9 => Some(format!("\x1b]9;{text}\x07").into_bytes()),
            NotifyMethod::Osc777 => {
                Some(format!("\x1b]777;notify;Fontaine;{text}\x07").into_bytes())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::denials::UnavailablePrompt;
    use fontaine_core::permissions::{ApprovalRequest, Offers, UnavailableChoice};
    use tokio::sync::oneshot;

    const APPROVAL: Option<(PromptKind, u64)> = Some((PromptKind::Approval, 1));

    fn bytes(method: NotifyMethod) -> Option<Vec<u8>> {
        Notifier::new(method).observe(APPROVAL)
    }

    #[test]
    fn each_method_writes_its_exact_bytes() {
        assert_eq!(bytes(NotifyMethod::Bell).unwrap(), b"\x07");
        assert_eq!(
            bytes(NotifyMethod::Osc9).unwrap(),
            b"\x1b]9;Fontaine needs your approval\x07"
        );
        assert_eq!(
            bytes(NotifyMethod::Osc777).unwrap(),
            b"\x1b]777;notify;Fontaine;Fontaine needs your approval\x07"
        );
        assert_eq!(bytes(NotifyMethod::Off), None);
    }

    #[test]
    fn a_prompt_signals_once_and_none_rearms() {
        let mut n = Notifier::new(NotifyMethod::Bell);
        assert!(n.observe(APPROVAL).is_some());
        assert!(n.observe(APPROVAL).is_none());
        assert!(n.observe(Some((PromptKind::Approval, 2))).is_some());
        assert!(n.observe(None).is_none());
        assert!(n.observe(Some((PromptKind::Approval, 2))).is_some());
    }

    fn app() -> App {
        App::new(fontaine_core::config::PermissionMode::Manual, "m".into())
    }

    fn request() -> ApprovalRequest {
        ApprovalRequest {
            tool: "shell".into(),
            agent: "main".into(),
            agent_name: String::new(),
            tool_use_id: None,
            args: serde_json::json!({ "command": "ls" }),
            summary: "ls".into(),
            tier: fontaine_core::tier::Tier::CriticalPath,
            reason: "needs approval".into(),
            offers: Offers::default(),
            proposed_rule: None,
            preview: None,
        }
    }

    fn unavailable(reply: oneshot::Sender<UnavailableChoice>) -> UnavailablePrompt {
        let info = fontaine_core::permissions::GuardianUnavailable {
            model: "a/b".into(),
            reason: "no key".into(),
            tool: "shell".into(),
        };
        UnavailablePrompt::new(info, Vec::new(), reply)
    }

    #[test]
    fn no_panel_alone_is_a_pending_prompt() {
        let mut app = app();
        assert_eq!(app.pending_prompt(), None);
        app.agents = Some(crate::agents_panel::AgentsPanel::default());
        assert_eq!(app.pending_prompt(), None);
    }

    #[test]
    fn each_begin_gives_its_kind_and_id() {
        let mut app = app();
        let (tx, _rx) = oneshot::channel();
        app.begin_approval(request(), tx);
        assert_eq!(app.pending_prompt(), Some((PromptKind::Approval, 1)));
        app.begin_guardian_off();
        assert_eq!(app.pending_prompt(), Some((PromptKind::GuardianOff, 1)));
        let (tx, _rx) = oneshot::channel();
        app.begin_unavailable(unavailable(tx));
        assert_eq!(app.pending_prompt(), Some((PromptKind::Unavailable, 1)));
    }

    #[test]
    fn an_approval_does_not_signal_again_after_a_prompt_over_it_is_answered() {
        let mut app = app();
        let mut n = Notifier::new(NotifyMethod::Bell);
        let (tx, _rx) = oneshot::channel();
        app.begin_approval(request(), tx);
        assert!(n.observe(app.pending_prompt()).is_some());
        let (tx, _rx) = oneshot::channel();
        app.begin_unavailable(unavailable(tx));
        assert!(n.observe(app.pending_prompt()).is_some());
        app.unavailable = None;
        assert!(n.observe(app.pending_prompt()).is_none());
    }

    #[test]
    fn a_prompt_under_the_agents_panel_signals_and_closing_it_does_not_again() {
        let mut app = app();
        let mut n = Notifier::new(NotifyMethod::Bell);
        app.agents = Some(crate::agents_panel::AgentsPanel::default());
        assert!(n.observe(app.pending_prompt()).is_none());
        let (tx, _rx) = oneshot::channel();
        app.begin_approval(request(), tx);
        assert!(n.observe(app.pending_prompt()).is_some());
        app.agents = None;
        assert!(n.observe(app.pending_prompt()).is_none());
    }
}
