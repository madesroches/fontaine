//! How a subagent's permission mode follows its parent's. The session ladder is
//! `dontAsk < manual < acceptEdits < auto`; `plan` is off the ladder, a session-root posture
//! that child agents inherit only as a ban on edits.

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

/// The root session's mode, as far as children care.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootMode {
    Plan,
    Ladder(Ladder),
}

/// How the pipeline treats one agent's call right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Posture {
    pub mode: Ladder,
    /// Edits are structurally blocked, as under a `plan` ancestor.
    pub plan_block: bool,
}

impl Posture {
    /// The root's posture. In `plan` the non-edit calls of a child are judged as `manual` at
    /// most, so the root stands for that here.
    pub fn of_root(root: RootMode) -> Posture {
        match root {
            RootMode::Plan => Posture {
                mode: Ladder::Manual,
                plan_block: true,
            },
            RootMode::Ladder(mode) => Posture {
                mode,
                plan_block: false,
            },
        }
    }

    /// A child's posture under `parent`, recomputed whenever the parent's changes: never
    /// looser than the parent, and under a `plan` ancestor never allowed to edit.
    pub fn child(own: Ladder, parent: Posture) -> Posture {
        Posture {
            mode: own.min(parent.mode),
            plan_block: parent.plan_block,
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
        let parent = Posture::of_root(RootMode::Ladder(Ladder::Manual));
        assert_eq!(own_mode(ModeSetting::Inherit, parent), Ladder::Manual);
        let auto = Posture::of_root(RootMode::Ladder(Ladder::Auto));
        let own = own_mode(ModeSetting::Inherit, auto);
        assert_eq!(own, Ladder::Auto);
        // The parent drops to manual later: the child follows.
        assert_eq!(Posture::child(own, parent).mode, Ladder::Manual);
        // And back up: the child's own mode is the ceiling.
        assert_eq!(Posture::child(own, auto).mode, Ladder::Auto);
    }

    #[test]
    fn an_explicit_mode_never_exceeds_the_parent() {
        let dont_ask = Posture::of_root(RootMode::Ladder(Ladder::DontAsk));
        assert_eq!(own_mode(ModeSetting::Manual, dont_ask), Ladder::DontAsk);
        let auto = Posture::of_root(RootMode::Ladder(Ladder::Auto));
        assert_eq!(own_mode(ModeSetting::Manual, auto), Ladder::Manual);
        assert_eq!(own_mode(ModeSetting::DontAsk, auto), Ladder::DontAsk);
    }

    #[test]
    fn under_plan_edits_are_blocked_and_the_rest_is_manual_at_most() {
        let plan = Posture::of_root(RootMode::Plan);
        let child = Posture::child(Ladder::Auto, plan);
        assert_eq!(
            child,
            Posture {
                mode: Ladder::Manual,
                plan_block: true
            }
        );
        let accept = Posture::child(Ladder::AcceptEdits, plan);
        assert_eq!(accept.mode, Ladder::Manual);
        let strict = Posture::child(Ladder::DontAsk, plan);
        assert_eq!(strict.mode, Ladder::DontAsk);
        // A grandchild inherits the ban through its parent.
        assert!(Posture::child(Ladder::Auto, child).plan_block);
    }
}
