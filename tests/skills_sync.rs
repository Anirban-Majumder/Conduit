#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

use std::fs;
use std::path::PathBuf;

use conduit::daemon::config::toml::SyncMode;
use conduit::daemon::skills::sync;

fn fresh_tmp(label: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("conduit-skills-sync-{label}-{ns}"))
}

/// SK-1: `sync_to_target` must refuse to overwrite a dir it didn't create.
#[test]
fn sync_to_target_refuses_unmanaged_destination() {
    let root = fresh_tmp("unmanaged");
    let src = root.join("src").join("my-skill");
    let dest_dir = root.join("dest");
    let dest = dest_dir.join("my-skill");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("SKILL.md"), "---\nname: My Skill\n---\n").unwrap();
    // Pre-existing user-authored dir at the destination.
    fs::create_dir_all(&dest).unwrap();
    fs::write(dest.join("user-file.txt"), "user content").unwrap();

    // we_manage=false → must return an error, must NOT modify the user dir.
    let result = sync::sync_to_target(&src, &dest_dir, "my-skill", SyncMode::Copy, false);
    assert!(
        result.is_err(),
        "sync_to_target must refuse to overwrite an unmanaged destination"
    );
    // User file must still be there.
    assert!(
        dest.join("user-file.txt").exists(),
        "user-authored file must not be deleted"
    );

    let _ignored = fs::remove_dir_all(&root);
}

/// SK-1: `sync_to_target` must overwrite when `we_manage=true` (Conduit owns it).
#[test]
fn sync_to_target_overwrites_managed_destination() {
    let root = fresh_tmp("managed");
    let src = root.join("src").join("my-skill");
    let dest_dir = root.join("dest");
    let dest = dest_dir.join("my-skill");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("SKILL.md"), "---\nname: My Skill\n---\n").unwrap();
    fs::write(src.join("new.txt"), "updated").unwrap();
    // Pre-existing managed dir.
    fs::create_dir_all(&dest).unwrap();
    fs::write(dest.join("old.txt"), "old content").unwrap();

    // we_manage=true → must succeed and replace.
    sync::sync_to_target(&src, &dest_dir, "my-skill", SyncMode::Copy, true).unwrap();
    assert!(dest.join("SKILL.md").exists(), "SKILL.md must be synced");
    assert!(dest.join("new.txt").exists(), "new file must appear");
    assert!(
        !dest.join("old.txt").exists(),
        "old file not in source must be removed"
    );

    let _ignored = fs::remove_dir_all(&root);
}

#[test]
fn copy_dir_all_mirrors_a_skill_directory() {
    let root = fresh_tmp("copy");
    let src = root.join("source").join("my-skill");
    let dst = root.join("dest").join("my-skill");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(src.join("scripts")).unwrap();
    fs::write(
        src.join("SKILL.md"),
        "---\nname: My Skill\ndescription: hi\n---\n",
    )
    .unwrap();
    fs::write(src.join("scripts").join("run.sh"), "#!/bin/sh\n").unwrap();

    sync::copy_dir_all(&src, &dst).unwrap();
    assert!(dst.join("SKILL.md").is_file());
    assert!(dst.join("scripts").join("run.sh").is_file());

    let _ignored = fs::remove_dir_all(&root);
}
