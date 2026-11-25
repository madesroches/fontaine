//! How a subagent's permission mode follows its parent's. The session ladder is
//! `dontAsk < manual < acceptEdits < auto`.

/// The modes a child agent can run in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ladder {
    DontAsk,
    Manual,
    AcceptEdits,
    Auto,
}

/// What an agent type's `mode` key says. An explicit mode can only be `manual` or `dontAsk`;
/// `acceptEdits` and `auto` are reachable for a child only through `Inherit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeSetting {
    Inherit,
    Manual,
    DontAsk,
}

/// How the pipeline treats one agent's call right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Posture {
    pub mode: Ladder,
}

impl Posture {
    /// A child's posture under `parent`, recomputed whenever the parent's changes: never
    /// looser than the parent.
    pub fn child(own: Ladder, parent: Posture) -> Posture {
        Posture {
            mode: own.min(parent.mode),
        }
    }
}

impl ModeSetting {
    /// The loosest mode a child of this type may ever run in; its effective mode is this
    /// capped by its parent's current posture.
    pub fn ceiling(self) -> Ladder {
        match self {
            ModeSetting::Inherit => Ladder::Auto,
            ModeSetting::Manual => Ladder::Manual,
            ModeSetting::DontAsk => Ladder::DontAsk,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(mode: Ladder) -> Posture {
        Posture { mode }
    }

    #[test]
    fn an_inherit_child_follows_the_parent_both_ways() {
        let ceiling = ModeSetting::Inherit.ceiling();
        assert_eq!(
            Posture::child(ceiling, at(Ladder::AcceptEdits)).mode,
            Ladder::AcceptEdits
        );
        assert_eq!(Posture::child(ceiling, at(Ladder::Auto)).mode, Ladder::Auto);
        assert_eq!(
            Posture::child(ceiling, at(Ladder::Manual)).mode,
            Ladder::Manual
        );
    }

    #[test]
    fn an_explicit_mode_is_a_ceiling_under_the_parent() {
        let manual = ModeSetting::Manual.ceiling();
        assert_eq!(
            Posture::child(manual, at(Ladder::DontAsk)).mode,
            Ladder::DontAsk
        );
        assert_eq!(
            Posture::child(manual, at(Ladder::Auto)).mode,
            Ladder::Manual
        );
        let dont_ask = ModeSetting::DontAsk.ceiling();
        for parent in [Ladder::Manual, Ladder::AcceptEdits, Ladder::Auto] {
            assert_eq!(Posture::child(dont_ask, at(parent)).mode, Ladder::DontAsk);
        }
    }
}
