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

/// The mode a child starts with: its type's setting resolved against the parent's posture at
/// spawn. `Inherit` takes the parent's mode as it is then; an explicit mode is capped by it.
pub fn own_mode(setting: ModeSetting, parent: Posture) -> Ladder {
    match setting {
        ModeSetting::Inherit => parent.mode,
        ModeSetting::Manual => Ladder::Manual.min(parent.mode),
        ModeSetting::DontAsk => Ladder::DontAsk,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_is_never_looser_than_its_parent() {
        let parent = Posture {
            mode: Ladder::Manual,
        };
        assert_eq!(own_mode(ModeSetting::Inherit, parent), Ladder::Manual);
        let auto = Posture { mode: Ladder::Auto };
        let own = own_mode(ModeSetting::Inherit, auto);
        assert_eq!(own, Ladder::Auto);
        // The parent drops to manual later: the child follows.
        assert_eq!(Posture::child(own, parent).mode, Ladder::Manual);
        // And back up: the child's own mode is the ceiling.
        assert_eq!(Posture::child(own, auto).mode, Ladder::Auto);
    }

    #[test]
    fn an_explicit_mode_never_exceeds_the_parent() {
        let dont_ask = Posture {
            mode: Ladder::DontAsk,
        };
        assert_eq!(own_mode(ModeSetting::Manual, dont_ask), Ladder::DontAsk);
        let auto = Posture { mode: Ladder::Auto };
        assert_eq!(own_mode(ModeSetting::Manual, auto), Ladder::Manual);
        assert_eq!(own_mode(ModeSetting::DontAsk, auto), Ladder::DontAsk);
    }
}
