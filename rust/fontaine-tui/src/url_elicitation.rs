//! The dialog for an MCP server's request to open a link. The URL is untrusted server text:
//! it is shown and handed to the system's browser only after the person confirms, and only an
//! `http`/`https` URL is ever opened.

use std::sync::Arc;
use std::time::Instant;

use fontaine_tools::mcp::{ElicitReply, ElicitUrl, openable_url};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use tokio::sync::oneshot;

use crate::confirm_prompt::ANSWER_GRACE;
use crate::input::wrap;
use crate::question::clean_multiline;
use crate::theme::Theme;

/// Where a warning about the link, such as an open failure, is reported.
pub(crate) type WarnSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Opens a validated URL in the person's default browser; a stub stands in for it in tests.
#[async_trait::async_trait]
pub trait UrlOpener: Send + Sync {
    async fn open(&self, url: &str) -> Result<(), String>;
}

/// The system opener: the `open` crate knows the platform's `xdg-open` / `open` / `start`.
pub struct DefaultUrlOpener;

#[async_trait::async_trait]
impl UrlOpener for DefaultUrlOpener {
    async fn open(&self, url: &str) -> Result<(), String> {
        let url = url.to_string();
        tokio::task::spawn_blocking(move || open::that_detached(url).map_err(|e| e.to_string()))
            .await
            .map_err(|e| e.to_string())?
    }
}

/// What a key press did to an elicitation dialog.
pub enum ElicitKey {
    /// The dialog stays open.
    Open,
    /// The dialog closed with this reply.
    Done(ElicitReply),
    /// The dialog closed; the reply follows once the link open finishes.
    Pending,
}

pub struct UrlElicitationPanel {
    /// Set by the app; tells two consecutive dialogs apart.
    pub(crate) id: u64,
    server: String,
    message: String,
    url: String,
    opener: Arc<dyn UrlOpener>,
    warn: WarnSink,
    reply: Option<oneshot::Sender<ElicitReply>>,
    opened: Instant,
}

impl UrlElicitationPanel {
    pub fn new(request: ElicitUrl, reply: oneshot::Sender<ElicitReply>, warn: WarnSink) -> Self {
        Self::with_opener(request, reply, warn, Arc::new(DefaultUrlOpener))
    }

    pub fn with_opener(
        request: ElicitUrl,
        reply: oneshot::Sender<ElicitReply>,
        warn: WarnSink,
        opener: Arc<dyn UrlOpener>,
    ) -> Self {
        UrlElicitationPanel {
            id: 0,
            server: clean_multiline(&request.server).replace('\n', " "),
            message: clean_multiline(&request.message),
            url: request.url,
            opener,
            warn,
            reply: Some(reply),
            opened: Instant::now(),
        }
    }

    /// Whether nobody waits for the answer any more, such as when the call ended.
    pub fn is_closed(&self) -> bool {
        self.reply.as_ref().is_none_or(oneshot::Sender::is_closed)
    }

    /// The dialog holds no field; the URL stands on its own line so it can be copied.
    pub fn field(&self) -> Option<(&'static str, String)> {
        None
    }

    /// Hands the answer to the waiting server connection. One nobody waits for is dropped.
    pub fn reply(&mut self, reply: ElicitReply) {
        if let Some(sender) = self.reply.take() {
            let _ = sender.send(reply);
        }
    }

    /// Lets a test answer at once instead of waiting out the grace period.
    #[cfg(test)]
    pub(crate) fn settle(&mut self) {
        self.opened = self
            .opened
            .checked_sub(ANSWER_GRACE * 2)
            .unwrap_or(self.opened);
    }

    /// A paste carries nothing the dialog can take; typed text goes nowhere.
    pub fn handle_paste(&mut self, _text: &str) {}

    pub fn handle_key(&mut self, key: KeyEvent) -> ElicitKey {
        self.handle_key_at(key, Instant::now())
    }

    /// Handles a key; `y` opens the link, `n`, Esc and Ctrl-D decline. The answer comes back
    /// from [`ElicitKey::Done`], or from the spawned open task after [`ElicitKey::Pending`].
    pub fn handle_key_at(&mut self, key: KeyEvent, now: Instant) -> ElicitKey {
        let answers = matches!(key.code, KeyCode::Char(_) | KeyCode::Esc);
        if answers
            && (key.kind == KeyEventKind::Repeat
                || now.saturating_duration_since(self.opened) < ANSWER_GRACE)
        {
            return ElicitKey::Open;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('y') if !control => self.open_link(),
            KeyCode::Char('n') if !control => ElicitKey::Done(ElicitReply::Decline),
            KeyCode::Esc => ElicitKey::Done(ElicitReply::Decline),
            KeyCode::Char('d') if control => ElicitKey::Done(ElicitReply::Decline),
            _ => ElicitKey::Open,
        }
    }

    /// Opens the link and answers once the open finishes: accept on success, decline on
    /// failure, with the failure also reported as a warning. The parsed URL string is what is
    /// handed over, never the raw server text.
    fn open_link(&mut self) -> ElicitKey {
        let Some(url) = openable_url(&self.url) else {
            (self.warn)("declined an MCP server's link: only http and https links are opened");
            return ElicitKey::Done(ElicitReply::Decline);
        };
        let Some(sender) = self.reply.take() else {
            return ElicitKey::Done(ElicitReply::Decline);
        };
        let opener = self.opener.clone();
        let warn = self.warn.clone();
        tokio::spawn(async move {
            let reply = match opener.open(&url).await {
                Ok(()) => ElicitReply::Accept(serde_json::Map::default()),
                Err(error) => {
                    (warn)(&format!("could not open the MCP server's link: {error}"));
                    ElicitReply::Decline
                }
            };
            let _ = sender.send(reply);
        });
        ElicitKey::Pending
    }

    /// The server's message, the URL in bold so it is visible and can be copied, and the
    /// external-site warning.
    pub fn body(&self, width: usize, max_rows: usize, theme: &Theme) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let width = width.max(8);
        let mut lines = vec![Line::styled(
            format!("MCP server `{}` wants to open a link", self.server),
            bold.fg(theme.heading),
        )];
        for line in self.message.lines() {
            for piece in wrap(line, width) {
                lines.push(Line::styled(piece, bold));
            }
        }
        for piece in wrap(&self.url, width) {
            lines.push(Line::styled(piece, bold));
        }
        lines.push(Line::styled(
            "This link goes to an external site chosen by the server.",
            dim,
        ));
        lines.truncate(max_rows.max(1));
        lines
    }

    pub fn footer(&self) -> Vec<Line<'static>> {
        vec![Line::styled(
            "y opens the link · n or Esc declines · Ctrl-D declines",
            Style::default().add_modifier(Modifier::DIM),
        )]
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use fontaine_tools::mcp::ElicitUrl;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;

    struct Stub {
        opened: Arc<Mutex<Vec<String>>>,
        result: Result<(), String>,
    }

    #[async_trait::async_trait]
    impl UrlOpener for Stub {
        async fn open(&self, url: &str) -> Result<(), String> {
            self.opened.lock().unwrap().push(url.to_string());
            self.result.clone()
        }
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn request(url: &str) -> ElicitUrl {
        ElicitUrl {
            server: "srv".into(),
            message: "Sign in\nplease".into(),
            url: url.into(),
        }
    }

    fn text_of(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn theme() -> Theme {
        Theme::preset(fontaine_core::config::ThemeName::default())
    }

    fn open(
        url: &str,
        result: Result<(), String>,
    ) -> (
        UrlElicitationPanel,
        oneshot::Receiver<ElicitReply>,
        Arc<Mutex<Vec<String>>>,
    ) {
        let (tx, rx) = oneshot::channel();
        let opened = Arc::new(Mutex::new(Vec::new()));
        let mut panel = UrlElicitationPanel::with_opener(
            request(url),
            tx,
            Arc::new(|_| {}),
            Arc::new(Stub {
                opened: opened.clone(),
                result,
            }),
        );
        panel.settle();
        (panel, rx, opened)
    }

    /// A `Done` reply is the caller's to send, as the app does; until then the panel still
    /// holds the sender and the receiver would wait forever.
    fn answer_like_the_app(panel: &mut UrlElicitationPanel, key: KeyEvent) {
        let ElicitKey::Done(reply) = panel.handle_key(key) else {
            panic!("the key answers at once");
        };
        assert_eq!(reply, ElicitReply::Decline);
        panel.reply(reply);
    }

    #[test]
    fn the_body_shows_the_server_the_message_the_url_and_the_warning() {
        let (panel, _rx, _opened) = open("https://example.com/auth", Ok(()));
        let shown = text_of(&panel.body(60, 20, &theme()));
        assert!(
            shown.contains("MCP server `srv` wants to open a link"),
            "{shown}"
        );
        assert!(shown.contains("Sign in\nplease"), "{shown}");
        assert!(shown.contains("https://example.com/auth"), "{shown}");
        assert!(
            shown.contains("This link goes to an external site chosen by the server."),
            "{shown}"
        );
        let footer = text_of(&panel.footer());
        assert!(
            footer.contains("y opens the link") && footer.contains("Ctrl-D declines"),
            "{footer}"
        );
    }

    #[tokio::test]
    async fn y_opens_the_link_and_accepts() {
        let (mut panel, rx, opened) = open("https://example.com/auth", Ok(()));
        assert!(matches!(
            panel.handle_key(press(KeyCode::Char('y'))),
            ElicitKey::Pending
        ));
        assert!(panel.is_closed());
        let reply = rx.await.expect("the open task answers");
        assert_eq!(reply, ElicitReply::Accept(serde_json::Map::default()));
        assert_eq!(
            opened.lock().unwrap().as_slice(),
            ["https://example.com/auth"]
        );
    }

    #[tokio::test]
    async fn the_opener_gets_the_parsed_url_not_the_raw_text() {
        let (mut panel, rx, opened) = open("https://example.com/a b", Ok(()));
        assert!(matches!(
            panel.handle_key(press(KeyCode::Char('y'))),
            ElicitKey::Pending
        ));
        rx.await.expect("the open task answers");
        assert_eq!(
            opened.lock().unwrap().as_slice(),
            ["https://example.com/a%20b"]
        );
    }

    #[tokio::test]
    async fn an_open_failure_declines_and_warns() {
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = oneshot::channel();
        let mut panel = UrlElicitationPanel::with_opener(
            request("https://example.com"),
            tx,
            {
                let warnings = warnings.clone();
                Arc::new(move |message: &str| warnings.lock().unwrap().push(message.to_string()))
            },
            Arc::new(Stub {
                opened: Arc::new(Mutex::new(Vec::new())),
                result: Err("no browser".into()),
            }),
        );
        panel.settle();
        assert!(matches!(
            panel.handle_key(press(KeyCode::Char('y'))),
            ElicitKey::Pending
        ));
        assert_eq!(rx.await, Ok(ElicitReply::Decline));
        let warned = warnings.lock().unwrap();
        assert_eq!(warned.len(), 1);
        assert!(warned[0].contains("no browser"), "{warned:?}");
    }

    #[tokio::test]
    async fn n_esc_and_ctrl_d_decline_without_opening() {
        for key in [press(KeyCode::Char('n')), press(KeyCode::Esc), ctrl('d')] {
            let (mut panel, rx, opened) = open("https://example.com", Ok(()));
            answer_like_the_app(&mut panel, key);
            assert_eq!(rx.await, Ok(ElicitReply::Decline));
            assert!(opened.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn an_unopenable_url_is_declined_without_opening() {
        let (mut panel, rx, opened) = open("file:///etc/passwd", Ok(()));
        answer_like_the_app(&mut panel, press(KeyCode::Char('y')));
        assert_eq!(rx.await, Ok(ElicitReply::Decline));
        assert!(opened.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn answer_keys_wait_out_the_grace_period() {
        let (tx, _rx) = oneshot::channel();
        let mut panel = UrlElicitationPanel::with_opener(
            request("https://example.com"),
            tx,
            Arc::new(|_| {}),
            Arc::new(Stub {
                opened: Arc::new(Mutex::new(Vec::new())),
                result: Ok(()),
            }),
        );
        let now = Instant::now();
        assert!(matches!(
            panel.handle_key_at(press(KeyCode::Char('y')), now),
            ElicitKey::Open
        ));
        assert!(matches!(
            panel.handle_key_at(press(KeyCode::Esc), now),
            ElicitKey::Open
        ));
        assert!(!panel.is_closed());
        panel.settle();
        assert!(matches!(
            panel.handle_key(press(KeyCode::Char('y'))),
            ElicitKey::Pending
        ));
    }
}
