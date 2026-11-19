use data_encoding::BASE32_NOPAD;
use iroh::EndpointAddr;
use serde::{Deserialize, Serialize};

pub const PREFIX: &str = "fontaine-attach1";
/// What the link and the QR code carry before the ticket body.
pub const LINK_PREFIX: &str = "fontaine://pair/";
/// The prefix of the one-shot tickets `fontaine send` used to mint; no build reads them now.
const RETIRED_PREFIX: &str = "fontaine-send1";
const TICKET_VERSION: u16 = 3;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ticket {
    pub v: u16,
    pub node: EndpointAddr,
    #[serde(with = "secret_text")]
    pub secret: [u8; 32],
    /// Unix seconds on the issuing machine's clock.
    pub expires_at: u64,
}

mod secret_text {
    use data_encoding::BASE32_NOPAD;
    use serde::{Deserialize, Deserializer, Serializer, de};

    pub fn serialize<S: Serializer>(secret: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&BASE32_NOPAD.encode(secret))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(d)?;
        let bytes = BASE32_NOPAD
            .decode(text.as_bytes())
            .map_err(de::Error::custom)?;
        bytes
            .try_into()
            .map_err(|_| de::Error::custom("secret is not 32 bytes"))
    }
}

/// The ticket as it is laid out in bytes; the secret stays raw here, where `Ticket`'s own serde
/// form writes it as text.
#[derive(Serialize, Deserialize)]
struct Wire {
    v: u16,
    node: EndpointAddr,
    secret: [u8; 32],
    expires_at: u64,
}

fn has_prefix(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

#[derive(Debug)]
pub enum TicketError {
    /// The text is none of the prefixed ticket, the link or a bare ticket body.
    NotATicket,
    /// A ticket of the removed `fontaine send`.
    Retired,
    Encoding(data_encoding::DecodeError),
    Binary(postcard::Error),
    UnsupportedVersion(u16),
}

impl std::fmt::Display for TicketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TicketError::NotATicket => write!(
                f,
                "not a fontaine ticket (expected `{PREFIX}...` or a `{LINK_PREFIX}...` link)"
            ),
            TicketError::Retired => f.write_str(
                "this is a ticket of the removed `fontaine send`; ask for a new one and use `fontaine attach`",
            ),
            TicketError::Encoding(_) => f.write_str("the ticket text is corrupted"),
            TicketError::Binary(_) => f.write_str("the ticket contents are malformed"),
            TicketError::UnsupportedVersion(v) => {
                write!(
                    f,
                    "the ticket is version {v}; this build reads version {TICKET_VERSION}"
                )
            }
        }
    }
}

impl std::error::Error for TicketError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TicketError::Encoding(e) => Some(e),
            TicketError::Binary(e) => Some(e),
            _ => None,
        }
    }
}

impl Ticket {
    pub fn new(node: EndpointAddr, secret: [u8; 32], expires_at: u64) -> Ticket {
        Ticket {
            v: TICKET_VERSION,
            node,
            secret,
            expires_at,
        }
    }

    /// The base32 text of the compact binary form, lowercase. The binary form (not JSON) keeps the
    /// text short enough for a QR code a terminal can show.
    fn body(&self) -> String {
        let wire = Wire {
            v: self.v,
            node: self.node.clone(),
            secret: self.secret,
            expires_at: self.expires_at,
        };
        let bytes = postcard::to_stdvec(&wire).expect("a ticket serializes");
        BASE32_NOPAD.encode(&bytes).to_lowercase()
    }

    /// One line, safe to paste: the prefix and the lowercase base32 body.
    pub fn encode(&self) -> String {
        format!("{PREFIX}{}", self.body())
    }

    /// The deep link a phone opens: the scheme and path say what it is, so the body carries no
    /// prefix.
    pub fn link(&self) -> String {
        format!("{LINK_PREFIX}{}", self.body())
    }

    /// What a QR code carries: the link with the body uppercased, which `parse` reads back
    /// unchanged. The scheme stays lowercase so Android matches it, and the uppercase body keeps
    /// the code smaller than lowercase would, since the QR encoder switches modes per segment.
    pub fn qr_payload(&self) -> String {
        format!("{LINK_PREFIX}{}", self.body().to_uppercase())
    }

    /// Reads the prefixed ticket, the link or the bare body, each without regard to case.
    pub fn parse(text: &str) -> Result<Ticket, TicketError> {
        let text = text.trim();
        if has_prefix(text, RETIRED_PREFIX) {
            return Err(TicketError::Retired);
        }
        if has_prefix(text, PREFIX) {
            return Self::from_body(&text[PREFIX.len()..]);
        }
        if has_prefix(text, LINK_PREFIX) {
            return Self::from_body(&text[LINK_PREFIX.len()..]);
        }
        // Bare text is ambiguous (a host selector looks like one), so only a body that decodes
        // to a ticket counts; anything else is not a ticket.
        let base32 = !text.is_empty()
            && text
                .bytes()
                .all(|b| b.is_ascii_alphabetic() || (b'2'..=b'7').contains(&b));
        if base32 {
            match Self::from_body(text) {
                Err(TicketError::Encoding(_) | TicketError::Binary(_)) => {}
                other => return other,
            }
        }
        Err(TicketError::NotATicket)
    }

    fn from_body(body: &str) -> Result<Ticket, TicketError> {
        let bytes = BASE32_NOPAD
            .decode(body.to_uppercase().as_bytes())
            .map_err(TicketError::Encoding)?;
        let wire: Wire = postcard::from_bytes(&bytes).map_err(TicketError::Binary)?;
        if wire.v != TICKET_VERSION {
            return Err(TicketError::UnsupportedVersion(wire.v));
        }
        Ok(Ticket {
            v: wire.v,
            node: wire.node,
            secret: wire.secret,
            expires_at: wire.expires_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    fn sample() -> Ticket {
        let key = SecretKey::from_bytes(&[7; 32]);
        let addr = EndpointAddr::new(key.public()).with_ip_addr("127.0.0.1:4000".parse().unwrap());
        Ticket::new(addr, [9; 32], 1_700_000_000)
    }

    #[test]
    fn a_ticket_round_trips_as_one_line() {
        let ticket = sample();
        let text = ticket.encode();
        assert!(text.starts_with(PREFIX) && !text.contains(char::is_whitespace));
        assert_eq!(Ticket::parse(&text).unwrap(), ticket);
        assert_eq!(Ticket::parse(&format!("  {text}\n")).unwrap(), ticket);
    }

    #[test]
    fn the_qr_payload_is_a_link_that_parses_back() {
        let ticket = sample();
        let payload = ticket.qr_payload();
        let body = payload.strip_prefix(LINK_PREFIX).expect("a link");
        assert!(
            body.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        );
        assert_eq!(Ticket::parse(&payload).unwrap(), ticket);
    }

    #[test]
    fn the_link_the_prefixed_ticket_and_the_bare_body_all_parse() {
        let ticket = sample();
        let link = ticket.link();
        assert!(link.starts_with(LINK_PREFIX) && !link.contains(PREFIX));
        let body = link.strip_prefix(LINK_PREFIX).unwrap();
        assert_eq!(Ticket::parse(&link).unwrap(), ticket);
        assert_eq!(
            Ticket::parse(&format!("{LINK_PREFIX}{}", body.to_uppercase())).unwrap(),
            ticket
        );
        assert_eq!(Ticket::parse(body).unwrap(), ticket);
        assert_eq!(Ticket::parse(&body.to_uppercase()).unwrap(), ticket);
        assert_eq!(Ticket::parse(&ticket.encode()).unwrap(), ticket);
        let mixed = format!("Fontaine-Attach1{}", body);
        assert_eq!(Ticket::parse(&mixed).unwrap(), ticket);
        assert_eq!(Ticket::parse(&format!(" {link}\n")).unwrap(), ticket);
    }

    #[test]
    fn a_wrong_scheme_or_path_is_not_a_ticket() {
        let body = sample().link().replace(LINK_PREFIX, "");
        for text in [
            format!("https://pair/{body}"),
            format!("fontaine://other/{body}"),
            format!("fontaine://{body}"),
            format!("fontaine:pair/{body}"),
            String::new(),
        ] {
            assert!(
                matches!(Ticket::parse(&text), Err(TicketError::NotATicket)),
                "{text}"
            );
        }
    }

    #[test]
    fn an_uppercase_retired_ticket_is_retired() {
        assert!(matches!(
            Ticket::parse("FONTAINE-SEND1MFRGGZDF"),
            Err(TicketError::Retired)
        ));
    }

    #[test]
    fn an_id_only_ticket_round_trips_and_stays_short() {
        let key = SecretKey::from_bytes(&[7; 32]);
        let ticket = Ticket::new(EndpointAddr::new(key.public()), [9; 32], 1_700_000_000);
        assert_eq!(Ticket::parse(&ticket.encode()).unwrap(), ticket);
        assert!(ticket.encode().len() <= 140, "{}", ticket.encode().len());
    }

    #[test]
    fn malformed_tickets_are_typed() {
        assert!(matches!(
            Ticket::parse("hello"),
            Err(TicketError::NotATicket)
        ));
        assert!(TicketError::NotATicket.to_string().contains(LINK_PREFIX));
        assert!(matches!(
            Ticket::parse(&format!("{LINK_PREFIX}!!!")),
            Err(TicketError::Encoding(_))
        ));
        assert!(matches!(
            Ticket::parse("fontaine-send1mfrggzdf"),
            Err(TicketError::Retired)
        ));
        assert!(matches!(
            Ticket::parse("fontaine-attach1!!!"),
            Err(TicketError::Encoding(_))
        ));
        let junk = format!("{PREFIX}{}", BASE32_NOPAD.encode(b"{}").to_lowercase());
        assert!(matches!(Ticket::parse(&junk), Err(TicketError::Binary(_))));
        let mut ticket = sample();
        ticket.v = 9;
        assert!(matches!(
            Ticket::parse(&ticket.encode()),
            Err(TicketError::UnsupportedVersion(9))
        ));
    }

    // #708: the JSON form of a ticket with a relay and three addresses was ~590 characters, a QR
    // code of 39 rows that did not fit an ordinary terminal.
    #[test]
    fn a_ticket_with_a_relay_and_three_addresses_stays_small() {
        let key = SecretKey::from_bytes(&[7; 32]);
        let addr = EndpointAddr::new(key.public())
            .with_relay_url(
                "https://use1-1.relay.n0.iroh-canary.iroh.link./"
                    .parse()
                    .unwrap(),
            )
            .with_ip_addr("192.168.1.20:51234".parse().unwrap())
            .with_ip_addr(
                "[2607:fea8:1234:5678:aaaa:bbbb:cccc:dddd]:51234"
                    .parse()
                    .unwrap(),
            )
            .with_ip_addr("172.17.0.1:51234".parse().unwrap());
        let ticket = Ticket::new(addr, [9; 32], 1_700_000_000);
        let text = ticket.encode();
        assert!(text.len() < 300, "{} characters", text.len());
        assert_eq!(Ticket::parse(&text).unwrap(), ticket);
    }
}
