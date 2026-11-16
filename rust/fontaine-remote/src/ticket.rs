use data_encoding::BASE32_NOPAD;
use iroh::EndpointAddr;
use serde::{Deserialize, Serialize};

pub const PREFIX: &str = "fontaine-attach1";
/// The prefix of the one-shot tickets `fontaine send` used to mint; no build reads them now.
const RETIRED_PREFIX: &str = "fontaine-send1";
const TICKET_VERSION: u16 = 2;

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

#[derive(Debug)]
pub enum TicketError {
    /// The text does not start with the ticket prefix.
    NotATicket,
    /// A ticket of the removed `fontaine send`.
    Retired,
    Encoding(data_encoding::DecodeError),
    Json(serde_json::Error),
    UnsupportedVersion(u16),
}

impl std::fmt::Display for TicketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TicketError::NotATicket => write!(f, "not a fontaine ticket (expected `{PREFIX}...`)"),
            TicketError::Retired => f.write_str(
                "this is a ticket of the removed `fontaine send`; ask for a new one and use `fontaine attach`",
            ),
            TicketError::Encoding(_) => f.write_str("the ticket text is corrupted"),
            TicketError::Json(_) => f.write_str("the ticket contents are malformed"),
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
            TicketError::Json(e) => Some(e),
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

    /// One line, safe to paste: the prefix and lowercase base32 of the JSON.
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("a ticket serializes");
        format!("{PREFIX}{}", BASE32_NOPAD.encode(&json).to_lowercase())
    }

    pub fn parse(text: &str) -> Result<Ticket, TicketError> {
        let text = text.trim();
        if text.starts_with(RETIRED_PREFIX) {
            return Err(TicketError::Retired);
        }
        let rest = text.strip_prefix(PREFIX).ok_or(TicketError::NotATicket)?;
        let bytes = BASE32_NOPAD
            .decode(rest.to_uppercase().as_bytes())
            .map_err(TicketError::Encoding)?;
        let ticket: Ticket = serde_json::from_slice(&bytes).map_err(TicketError::Json)?;
        if ticket.v != TICKET_VERSION {
            return Err(TicketError::UnsupportedVersion(ticket.v));
        }
        Ok(ticket)
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
    fn malformed_tickets_are_typed() {
        assert!(matches!(
            Ticket::parse("hello"),
            Err(TicketError::NotATicket)
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
        assert!(matches!(Ticket::parse(&junk), Err(TicketError::Json(_))));
        let mut ticket = sample();
        ticket.v = 9;
        assert!(matches!(
            Ticket::parse(&ticket.encode()),
            Err(TicketError::UnsupportedVersion(9))
        ));
    }
}
