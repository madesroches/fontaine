use std::error::Error;

/// The error's message followed by each `source()` link whose text isn't already shown, joined
/// with `: `. Wrappers often repeat their source in their own message.
pub fn chain(error: &(dyn Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;

    #[derive(Debug)]
    struct Link(&'static str, Option<Box<Link>>);

    impl fmt::Display for Link {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.0)
        }
    }

    impl Error for Link {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            self.1.as_deref().map(|l| l as &(dyn Error + 'static))
        }
    }

    #[test]
    fn an_error_without_a_source_is_its_message() {
        assert_eq!(chain(&Link("boom", None)), "boom");
    }

    #[test]
    fn every_level_of_the_chain_is_shown() {
        let error = Link(
            "a",
            Some(Box::new(Link("b", Some(Box::new(Link("c", None)))))),
        );
        assert_eq!(chain(&error), "a: b: c");
    }

    #[test]
    fn a_source_the_message_already_repeats_is_not_repeated() {
        let error = Link("cannot read: denied", Some(Box::new(Link("denied", None))));
        assert_eq!(chain(&error), "cannot read: denied");
    }
}
