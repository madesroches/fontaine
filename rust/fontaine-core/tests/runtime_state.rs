//! What the runtime reports about the mode and the workspace roots while a session changes them.

mod common;

use common::World;
use common::runtime::build;
use fontaine_core::config::PermissionMode::{Auto, Manual, Plan};

#[tokio::test]
async fn the_mode_follows_the_state_while_the_start_mode_stays() {
    let world = World::new();
    let built = build(&world, Manual, Vec::new(), false);
    assert_eq!(built.runtime.mode(), Manual);

    built.runtime.state().set_mode(Plan);
    assert_eq!(built.runtime.mode(), Plan);
    built.runtime.state().set_mode(Auto);
    assert_eq!(built.runtime.mode(), Auto);
    assert_eq!(built.runtime.start_mode(), Manual);
}

#[tokio::test]
async fn add_dir_extends_the_roots() {
    let world = World::new();
    let mut built = build(&world, Manual, Vec::new(), false);
    assert_eq!(built.runtime.roots(), vec![world.work.clone()]);

    let extra = world.root.join("extra");
    std::fs::create_dir_all(&extra).unwrap();
    let added = built.runtime.add_dir(&extra, &world.work).unwrap();

    assert_eq!(built.runtime.roots(), vec![world.work.clone(), added]);
    assert_eq!(built.runtime.primary_root(), world.work);
}
