use jackdaw_project_build::legacy_schema::SourceScan;
use std::{fs, path::Path};

fn inventory(root: &Path, source: &Path) {
    let dir = root.join(".jackdaw/schema-probe");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("source-inputs.json"),
        serde_json::to_vec(&[source]).unwrap(),
    )
    .unwrap();
}

#[test]
fn passive_checks_accept_local_parent_segments_and_notice_source_changes() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    fs::create_dir_all(root.join("vendor/adapter")).unwrap();
    let src = root.join("plugins/example/src");
    fs::create_dir_all(&src).unwrap();
    let file = src.join("lib.rs");
    fs::write(&file, "pub struct A;").unwrap();
    inventory(root, &root.join("vendor/adapter/../../plugins/example/src"));
    let mut scan = SourceScan::default();
    let first = scan.metadata_stamp(root).unwrap();
    assert_eq!(first, scan.metadata_stamp(root).unwrap());
    fs::write(file, "pub struct A(pub f32);").unwrap();
    assert_ne!(first, scan.metadata_stamp(root).unwrap());
}

#[test]
fn passive_checks_refuse_a_project_inventory_that_escapes_its_root() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().join("project");
    let outside = scratch.path().join("other");
    fs::create_dir_all(&outside).unwrap();
    inventory(&root, &root.join("../other"));
    assert!(SourceScan::default().metadata_stamp(&root).is_err());
    inventory(&root, &outside);
    assert!(SourceScan::default().metadata_stamp(&root).is_err());
}
