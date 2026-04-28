// Integration test exercising plan execute + undo end-to-end.
// Cargo test target — separate crate from the binary, so we read the JSON
// shape the same way an external user would.

use std::fs;
use std::path::PathBuf;

#[test]
fn plan_executes_and_undoes() {
    use tidyrs::actions;
    use tidyrs::plan::{Action, Plan};

    let d = tempfile::TempDir::new().unwrap();
    let orig = d.path().join("orig.bin");
    let dup1 = d.path().join("dup1.bin");
    let dup2 = d.path().join("dup2.bin");
    let folded_a = d.path().join("a.txt");
    let folded_b = d.path().join("b.txt");

    fs::write(&orig, b"keep me").unwrap();
    fs::write(&dup1, b"keep me").unwrap();
    fs::write(&dup2, b"keep me").unwrap();
    fs::write(&folded_a, b"alpha").unwrap();
    fs::write(&folded_b, b"beta").unwrap();

    // Build a plan with both action kinds
    let plan = Plan {
        dry_run: false,
        actions: vec![
            Action::KeepOne {
                keep: orig.clone(),
                trash: vec![dup1.clone(), dup2.clone()],
            },
            Action::FoldIntoFolder {
                folder_name: "letters".to_string(),
                files: vec![folded_a.clone(), folded_b.clone()],
            },
        ],
    };

    // Round-trip through JSON (same path as --save-plan + --apply-plan)
    let json = plan.to_json().unwrap();
    let plan_path = d.path().join("plan.json");
    fs::write(&plan_path, json).unwrap();
    let loaded = Plan::from_json(&fs::read_to_string(&plan_path).unwrap()).unwrap();

    // Execute
    let log = actions::execute(&loaded).unwrap();

    // Verify side effects
    assert!(orig.exists(), "kept file should remain");
    assert!(!dup1.exists(), "dup1 should be trashed");
    assert!(!dup2.exists(), "dup2 should be trashed");

    let letters = d.path().join("letters");
    assert!(letters.is_dir(), "letters folder created");
    assert!(letters.join("a.txt").exists(), "a.txt moved");
    assert!(letters.join("b.txt").exists(), "b.txt moved");
    assert!(!folded_a.exists(), "original a.txt gone");

    // Undo log has entries for trash + folder + moves
    assert!(log.entries.len() >= 4);

    // Undo (only reverses moves and dir creation; trash restore is best-effort/skipped)
    let report = actions::undo(&log).unwrap();
    assert!(report.reversed >= 2, "should reverse at least 2 moves");
    assert!(folded_a.exists(), "a.txt restored to original location");
    assert!(folded_b.exists(), "b.txt restored to original location");
    assert!(!letters.exists(), "letters folder removed");

    // The trash entries are reported as skipped_other (not auto-restored)
    assert_eq!(report.skipped_other.len(), 2);
}

#[test]
fn dry_run_makes_no_changes() {
    use tidyrs::actions;
    use tidyrs::plan::{Action, Plan};

    let d = tempfile::TempDir::new().unwrap();
    let f = d.path().join("file.txt");
    fs::write(&f, b"hi").unwrap();

    let plan = Plan {
        dry_run: true,
        actions: vec![Action::KeepOne {
            keep: PathBuf::from("/nope"),
            trash: vec![f.clone()],
        }],
    };

    let log = actions::execute(&plan).unwrap();
    assert!(log.entries.is_empty(), "dry run should record nothing");
    assert!(f.exists(), "file should still be there");
}
