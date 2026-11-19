//! The `fontaine://pair/<body>` deep link, which the system camera opens and the in-app scanner
//! reads. A link only asks to pair: it carries a bearer ticket, and the host-terminal confirmation
//! is what authorizes.

use fontaine_remote::{LINK_PREFIX, Ticket, TicketError, short_node};

/// A parsed link, holding only what the confirmation screen shows and the ticket to connect with.
pub struct PairLink {
    pub ticket: Box<Ticket>,
    /// The leading characters of the host's node id, for the "Pair with ...?" prompt.
    pub host: String,
}

/// Reads a link, a prefixed ticket or a bare body, the three forms `Ticket::parse` accepts.
pub fn parse(text: &str) -> Result<PairLink, TicketError> {
    let ticket = Ticket::parse(text)?;
    let host = short_node(&ticket.node.id.to_string());
    Ok(PairLink {
        ticket: Box::new(ticket),
        host,
    })
}

/// Whether `text` starts like a pairing link; the activity uses it to tell a pairing intent from
/// any other VIEW intent before it hands the text over.
pub fn is_pair_link(text: &str) -> bool {
    text.trim()
        .get(..LINK_PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(LINK_PREFIX))
}

#[cfg(test)]
mod tests {
    use fontaine_remote::{EndpointAddr, SecretKey};

    use super::*;

    fn ticket() -> Ticket {
        let key = SecretKey::from_bytes(&[5; 32]);
        Ticket::new(EndpointAddr::new(key.public()), [3; 32], 1_700_000_000)
    }

    #[test]
    fn every_ticket_form_parses_to_the_same_host() {
        let ticket = ticket();
        let node = ticket.node.id.to_string();
        for text in [ticket.link(), ticket.qr_payload(), ticket.encode()] {
            let link = parse(&text).expect(&text);
            assert_eq!(*link.ticket, ticket);
            assert_eq!(link.host, node.chars().take(10).collect::<String>());
        }
    }

    #[test]
    fn other_text_is_not_a_link() {
        assert!(parse("https://example.com/pair/abc").is_err());
        assert!(parse("fontaine://other/abc").is_err());
        assert!(!is_pair_link("https://example.com"));
        assert!(is_pair_link(&ticket().qr_payload()));
        assert!(is_pair_link("  FONTAINE://PAIR/abc"));
    }
}
