use agentshub_app::catalog::Catalog;
use agentshub_core::agent::Agent;
use agentshub_core::store::{Change, Db, Store};

use crate::runtime::apply_change;

#[test]
fn a_change_reaches_the_catalog_before_it_is_saved() {
    let db = Db::open_in_memory().unwrap();
    let project = db
        .add_project(&std::env::temp_dir().join("agentshub-project"))
        .unwrap();
    let first = db.add_chat(project, Agent::Claude, None).unwrap();
    let second = db.add_chat(project, Agent::Codex, None).unwrap();
    let store = Store::new(db).unwrap();
    let mut catalog = Catalog::new(smol::block_on(store.projects()).unwrap());
    // A chat is added on top.
    assert_eq!(catalog.projects()[0].chats[0].id, second);

    let saved = apply_change(
        &mut catalog,
        &store,
        Change::Pin {
            chat: first,
            pinned: true,
        },
    );
    assert_eq!(catalog.projects()[0].chats[0].id, first);
    assert_eq!(catalog.unsaved(), 1);

    smol::block_on(saved).unwrap();
    let listed = smol::block_on(store.projects()).unwrap();
    assert_eq!(listed[0].chats[0].id, first);
}
