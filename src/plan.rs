use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum GroupKind {
    Duplicate,
    NameCluster,
    DuplicateDir,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupFile {
    pub path: PathBuf,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub kind: GroupKind,
    pub files: Vec<GroupFile>,
    pub label: String,
    pub suggested: Action,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    KeepOne {
        keep: PathBuf,
        trash: Vec<PathBuf>,
    },
    DeleteAll {
        trash: Vec<PathBuf>,
    },
    FoldIntoFolder {
        folder_name: String,
        files: Vec<PathBuf>,
    },
    Ignore,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Plan {
    pub actions: Vec<Action>,
    pub dry_run: bool,
}

impl Plan {
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    pub fn from_json(s: &str) -> serde_json::Result<Self> {
        serde_json::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_json() {
        let plan = Plan {
            dry_run: false,
            actions: vec![
                Action::KeepOne {
                    keep: PathBuf::from("/a/keep.txt"),
                    trash: vec![PathBuf::from("/a/dup.txt")],
                },
                Action::FoldIntoFolder {
                    folder_name: "trip".into(),
                    files: vec![PathBuf::from("/a/trip_1.jpg")],
                },
                Action::Ignore,
            ],
        };
        let json = plan.to_json().unwrap();
        let back = Plan::from_json(&json).unwrap();
        assert_eq!(back.actions.len(), 3);
        assert_eq!(back.actions[0], plan.actions[0]);
    }
}
