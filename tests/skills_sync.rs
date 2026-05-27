#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

use std::fs;
use std::path::PathBuf;

use conduit::daemon::skills::sync;

fn fresh_tmp(label: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("conduit-skills-sync-{label}-{ns}"))
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
