//! The guard: a process under test cannot open a store in the default data directory, because a
//! row written there outlives the test and every later reader counts it as real. One test in the
//! file, and it is the only one that sets a variable, so libtest's threads cannot race it.

use letibot_tokencore::Store;

/// The whole guard, through the doors the daemon and every fixture go through: the default data
/// directory is refused by a test run and by the flag, the test's own directory never is, and a
/// word from the run lets a debug build mean the default store.
#[test]
fn a_store_in_the_default_data_directory_is_refused_under_test() {
    let root = std::env::temp_dir().join(format!("letibot-store-guard-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("data/letibot")).unwrap();
    std::fs::create_dir_all(root.join("scratch")).unwrap();
    // SAFETY: this test binary has one test, so nothing else here reads the environment.
    unsafe {
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        std::env::remove_var("LETIBOT_REFUSE_DEFAULT_STORE");
    }

    // Unsaid, the profile answers: a debug build is a test or a debugging session, so it needs no
    // ceremony from the agent running `cargo test`. A release build is the installed product.
    let unsaid = std::panic::catch_unwind(|| Store::open(&root.join("data/letibot/unsaid.db")));
    assert_eq!(
        unsaid.is_err(),
        cfg!(debug_assertions),
        "the build profile decides when the flag says nothing"
    );

    // SAFETY: as above.
    unsafe { std::env::set_var("LETIBOT_REFUSE_DEFAULT_STORE", "1") };
    let refused = std::panic::catch_unwind(|| {
        Store::open(&root.join("data/letibot/sessions.db")).map(|_| ())
    })
    .expect_err("the default data directory is refused");
    let said = refused
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    assert!(
        said.contains("LETIBOT_REFUSE_DEFAULT_STORE"),
        "the refusal says how to mean it: {said}"
    );
    assert!(
        !root.join("data/letibot/sessions.db").exists(),
        "nothing was created: the refusal comes before the open"
    );
    // The set's door, which is what a daemon is handed: a data DIRECTORY.
    let set_refused =
        std::panic::catch_unwind(|| Store::open_set(&root.join("data/letibot"), "abc"));
    assert!(set_refused.is_err(), "a data directory is refused too");

    // The test's own directory is the whole point: it opens, and its file is there.
    let own = Store::open(&root.join("scratch/sessions.db")).expect("a store of the test's own");
    assert_eq!(
        own.path().map(|p| p.file_name().unwrap().to_owned()),
        Some("sessions.db".into())
    );
    drop(own);

    // And the operator's own run: the same path, with the run saying this is meant.
    // SAFETY: as above.
    unsafe { std::env::set_var("LETIBOT_REFUSE_DEFAULT_STORE", "0") };
    Store::open(&root.join("data/letibot/sessions.db"))
        .expect("a person who means the default store may open it");

    // SAFETY: as above; the test is over.
    unsafe {
        std::env::remove_var("LETIBOT_REFUSE_DEFAULT_STORE");
        std::env::remove_var("XDG_DATA_HOME");
    }
    let _ = std::fs::remove_dir_all(&root);
}
