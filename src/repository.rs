use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub type RepositoryId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutKind {
    Primary,
    Linked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutProvenance {
    pub repository_id: RepositoryId,
    pub checkout_path: PathBuf,
    pub kind: CheckoutKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repository {
    pub id: RepositoryId,
    pub git_common_dir: PathBuf,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_base: Option<String>,
    pub checkout_workspace_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_focused_workspace_id: Option<String>,
}

impl Repository {
    pub fn display_label(&self) -> &str {
        self.custom_name.as_deref().unwrap_or(&self.label)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum SpaceRef {
    Repository(RepositoryId),
    StandaloneWorkspace(String),
}

pub(crate) fn stable_repository_id(common_dir: &Path) -> RepositoryId {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in common_dir.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("r{hash:016x}")
}

/// Rebuild membership from checkout identity while retaining repository metadata,
/// child order, and MRU focus. Membership never follows foreground pane cwd.
pub(crate) fn reconcile(
    workspaces: &mut [crate::workspace::Workspace],
    repositories: &mut Vec<Repository>,
    space_order: &mut Vec<SpaceRef>,
    active_workspace_id: Option<&str>,
) {
    let mut previous_by_key: HashMap<PathBuf, Repository> = std::mem::take(repositories)
        .into_iter()
        .map(|repository| (repository.git_common_dir.clone(), repository))
        .collect();
    let previous_order = std::mem::take(space_order);
    let mut grouped: HashMap<PathBuf, Vec<(String, PathBuf, CheckoutKind, String)>> =
        HashMap::new();
    let mut standalone = Vec::new();

    for workspace in workspaces.iter_mut() {
        // A compatibility membership can outlive the checkout at its path.
        // The background discovery cache is authoritative; persisted identity
        // remains a fallback only while the checkout path is unavailable.
        let discovered = workspace
            .git_space()
            .cloned()
            // Preserve persisted identity only while its path is unavailable.
            // An existing non-Git directory is affirmative evidence that stale
            // provenance must not attach a replacement checkout to an old repo.
            .or_else(|| {
                if workspace.identity_cwd.exists() {
                    return None;
                }
                let checkout = workspace.checkout.as_ref()?;
                let repository = previous_by_key
                    .values()
                    .find(|repository| repository.id == checkout.repository_id)?;
                Some(crate::workspace::GitSpaceMetadata {
                    key: repository.git_common_dir.display().to_string(),
                    checkout_key: checkout.checkout_path.display().to_string(),
                    repo_name: repository.label.clone(),
                    repo_root: checkout.checkout_path.clone(),
                    is_linked_worktree: checkout.kind == CheckoutKind::Linked,
                })
            })
            .or_else(|| {
                if workspace.identity_cwd.exists() {
                    return None;
                }
                workspace
                    .worktree_space()
                    .map(|membership| crate::workspace::GitSpaceMetadata {
                        key: membership.key.clone(),
                        checkout_key: membership.checkout_path.display().to_string(),
                        repo_name: membership.label.clone(),
                        repo_root: membership.checkout_path.clone(),
                        is_linked_worktree: membership.is_linked_worktree,
                    })
            });
        if let Some(space) = discovered {
            grouped.entry(PathBuf::from(&space.key)).or_default().push((
                workspace.id.clone(),
                crate::worktree::canonical_or_original(&space.repo_root),
                if space.is_linked_worktree {
                    CheckoutKind::Linked
                } else {
                    CheckoutKind::Primary
                },
                space.repo_name,
            ));
        } else {
            workspace.checkout = None;
            standalone.push(workspace.id.clone());
        }
    }

    let mut by_id = HashMap::new();
    for (common_dir, members) in grouped {
        let mut repository = previous_by_key
            .remove(&common_dir)
            .unwrap_or_else(|| Repository {
                id: stable_repository_id(&common_dir),
                git_common_dir: common_dir.clone(),
                label: members
                    .first()
                    .map(|member| member.3.clone())
                    .unwrap_or_else(|| "repo".into()),
                custom_name: None,
                preferred_base: None,
                checkout_workspace_ids: Vec::new(),
                last_focused_workspace_id: None,
            });
        let discovered_ids: HashSet<_> = members.iter().map(|member| member.0.as_str()).collect();
        let mut ordered = repository
            .checkout_workspace_ids
            .iter()
            .filter(|id| discovered_ids.contains(id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        for (id, ..) in &members {
            if !ordered.contains(id) {
                ordered.push(id.clone());
            }
        }
        // The live primary checkout is the repository root, not a reorderable
        // child. Keep it first even if an older persisted child order predates
        // repository-aware grouping.
        ordered.sort_by_key(|id| {
            if members
                .iter()
                .find(|member| &member.0 == id)
                .is_some_and(|member| member.2 == CheckoutKind::Primary)
            {
                0
            } else {
                1
            }
        });
        repository.checkout_workspace_ids = ordered;
        if let Some(active) = active_workspace_id.filter(|id| discovered_ids.contains(*id)) {
            repository.last_focused_workspace_id = Some(active.to_string());
        } else if repository
            .last_focused_workspace_id
            .as_deref()
            .is_none_or(|id| !discovered_ids.contains(id))
        {
            repository.last_focused_workspace_id =
                repository.checkout_workspace_ids.first().cloned();
        }
        for (workspace_id, checkout_path, kind, _) in members {
            if let Some(workspace) = workspaces
                .iter_mut()
                .find(|workspace| workspace.id == workspace_id)
            {
                workspace.checkout = Some(CheckoutProvenance {
                    repository_id: repository.id.clone(),
                    checkout_path: checkout_path.clone(),
                    kind,
                });
                // Keep the additive workspace/worktree API populated for one migration window.
                workspace.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
                    key: common_dir.display().to_string(),
                    label: repository.label.clone(),
                    repo_root: repository
                        .git_common_dir
                        .parent()
                        .unwrap_or(&checkout_path)
                        .to_path_buf(),
                    checkout_path,
                    is_linked_worktree: kind == CheckoutKind::Linked,
                });
            }
        }
        by_id.insert(repository.id.clone(), repository);
    }

    let repository_ids: HashSet<_> = by_id.keys().cloned().collect();
    let standalone_ids: HashSet<_> = standalone.into_iter().collect();
    let mut order = Vec::new();
    for item in previous_order {
        let valid = match &item {
            SpaceRef::Repository(id) => repository_ids.contains(id),
            SpaceRef::StandaloneWorkspace(id) => standalone_ids.contains(id),
        };
        if valid && !order.contains(&item) {
            order.push(item);
        }
    }
    for workspace in workspaces.iter() {
        let item = workspace.checkout.as_ref().map_or_else(
            || SpaceRef::StandaloneWorkspace(workspace.id.clone()),
            |checkout| SpaceRef::Repository(checkout.repository_id.clone()),
        );
        if !order.contains(&item) {
            order.push(item);
        }
    }
    *repositories = by_id.into_values().collect();
    repositories.sort_by_key(|repository| {
        order
            .iter()
            .position(|item| item == &SpaceRef::Repository(repository.id.clone()))
            .unwrap_or(usize::MAX)
    });
    *space_order = order;
}

/// Derive repository state while migrating snapshots that predate first-class repositories.
pub(crate) fn migrate_workspace_projection(
    workspaces: &[crate::persist::WorkspaceSnapshot],
) -> (Vec<Repository>, Vec<SpaceRef>) {
    let mut repositories = Vec::<Repository>::new();
    let mut order = Vec::new();
    for (index, workspace) in workspaces.iter().enumerate() {
        let workspace_id = workspace
            .id
            .clone()
            .unwrap_or_else(|| format!("legacy-w{}", index + 1));
        let identity = workspace.worktree_space.as_ref();
        let Some(identity) = identity else {
            order.push(SpaceRef::StandaloneWorkspace(workspace_id));
            continue;
        };
        let common_dir = PathBuf::from(&identity.key);
        let repository_id = stable_repository_id(&common_dir);
        if let Some(repository) = repositories
            .iter_mut()
            .find(|repo| repo.id == repository_id)
        {
            repository.checkout_workspace_ids.push(workspace_id);
        } else {
            repositories.push(Repository {
                id: repository_id.clone(),
                git_common_dir: common_dir,
                label: identity.label.clone(),
                custom_name: None,
                preferred_base: None,
                checkout_workspace_ids: vec![workspace_id.clone()],
                last_focused_workspace_id: Some(workspace_id),
            });
            order.push(SpaceRef::Repository(repository_id));
        }
    }
    (repositories, order)
}

/// Rebuild the shared order from the current flat compatibility order.
/// Repository members remain contiguous and the primary checkout stays first.
pub(crate) fn project_workspace_order(
    workspaces: &[crate::workspace::Workspace],
    repositories: &mut [Repository],
    space_order: &mut Vec<SpaceRef>,
) {
    for repository in repositories.iter_mut() {
        let mut members = workspaces
            .iter()
            .filter(|workspace| {
                workspace
                    .checkout
                    .as_ref()
                    .is_some_and(|checkout| checkout.repository_id == repository.id)
            })
            .collect::<Vec<_>>();
        members.sort_by_key(|workspace| {
            usize::from(
                workspace
                    .checkout
                    .as_ref()
                    .is_none_or(|checkout| checkout.kind != CheckoutKind::Primary),
            )
        });
        repository.checkout_workspace_ids = members
            .iter()
            .map(|workspace| workspace.id.clone())
            .collect();
    }
    let mut next = Vec::new();
    for workspace in workspaces {
        let item = workspace.checkout.as_ref().map_or_else(
            || SpaceRef::StandaloneWorkspace(workspace.id.clone()),
            |checkout| SpaceRef::Repository(checkout.repository_id.clone()),
        );
        if !next.contains(&item) {
            next.push(item);
        }
    }
    *space_order = next;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{GitSpaceMetadata, Workspace};

    fn checkout(name: &str, common: &str, root: &str, linked: bool) -> Workspace {
        let mut workspace = Workspace::test_new(name);
        workspace.identity_cwd = root.into();
        workspace.cached_git_space = Some(GitSpaceMetadata {
            key: common.into(),
            checkout_key: root.into(),
            repo_name: common.rsplit('/').next().unwrap_or("repo").into(),
            repo_root: root.into(),
            is_linked_worktree: linked,
        });
        workspace
    }

    fn init_git_repo(path: &Path) {
        std::fs::create_dir_all(path).expect("create test repo");
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(path)
            .status()
            .expect("run git init");
        assert!(status.success());
    }

    #[test]
    fn live_git_identity_replaces_stale_membership_at_the_same_path() {
        let path = std::env::temp_dir().join(format!(
            "herdr-repository-replacement-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        init_git_repo(&path);
        let mut workspace = Workspace::test_new("replacement");
        workspace.identity_cwd = path.clone();
        workspace.cached_git_space = crate::workspace::git_space_metadata(&path);
        workspace.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "/gone/old/.git".into(),
            label: "old".into(),
            repo_root: path.clone(),
            checkout_path: path.clone(),
            is_linked_worktree: false,
        });
        let old = Repository {
            id: "old-repository".into(),
            git_common_dir: PathBuf::from("/gone/old/.git"),
            label: "old".into(),
            custom_name: None,
            preferred_base: None,
            checkout_workspace_ids: vec![workspace.id.clone()],
            last_focused_workspace_id: None,
        };
        let mut workspaces = vec![workspace];
        let mut repositories = vec![old];
        let mut order = vec![SpaceRef::Repository("old-repository".into())];

        reconcile(&mut workspaces, &mut repositories, &mut order, None);

        assert_ne!(repositories[0].id, "old-repository");
        assert_ne!(
            repositories[0].git_common_dir,
            PathBuf::from("/gone/old/.git")
        );
        assert_eq!(
            workspaces[0].checkout.as_ref().unwrap().repository_id,
            repositories[0].id
        );
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn existing_non_git_directory_rejects_stale_checkout_identity() {
        let path = std::env::temp_dir().join(format!(
            "herdr-stale-checkout-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("create non-git directory");
        let mut workspace = Workspace::test_new("stale");
        workspace.identity_cwd = path.clone();
        workspace.checkout = Some(CheckoutProvenance {
            repository_id: "old".into(),
            checkout_path: path.clone(),
            kind: CheckoutKind::Primary,
        });
        let mut workspaces = vec![workspace];
        let mut repositories = vec![Repository {
            id: "old".into(),
            git_common_dir: path.join(".git"),
            label: "old".into(),
            custom_name: None,
            preferred_base: None,
            checkout_workspace_ids: vec![workspaces[0].id.clone()],
            last_focused_workspace_id: None,
        }];
        let mut order = vec![SpaceRef::Repository("old".into())];

        reconcile(&mut workspaces, &mut repositories, &mut order, None);

        assert!(workspaces[0].checkout.is_none());
        assert!(repositories.is_empty());
        assert_eq!(
            order,
            vec![SpaceRef::StandaloneWorkspace(workspaces[0].id.clone())]
        );
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn automatic_grouping_keeps_all_checkouts_equal_and_stable() {
        let mut workspaces = vec![
            checkout("main", "/repos/app/.git", "/repos/app", false),
            checkout("feature", "/repos/app/.git", "/worktrees/app-feature", true),
        ];
        let ids = workspaces
            .iter()
            .map(|workspace| workspace.id.clone())
            .collect::<Vec<_>>();
        let mut repositories = Vec::new();
        let mut order = Vec::new();
        reconcile(
            &mut workspaces,
            &mut repositories,
            &mut order,
            Some(&ids[1]),
        );

        assert_eq!(repositories.len(), 1);
        assert_eq!(repositories[0].checkout_workspace_ids, ids);
        assert_eq!(
            repositories[0].last_focused_workspace_id,
            Some(ids[1].clone())
        );
        assert_eq!(
            workspaces[0].checkout.as_ref().unwrap().kind,
            CheckoutKind::Primary
        );
        assert_eq!(
            workspaces[1].checkout.as_ref().unwrap().kind,
            CheckoutKind::Linked
        );
        assert_eq!(
            order,
            vec![SpaceRef::Repository(repositories[0].id.clone())]
        );
    }

    #[test]
    fn one_checkout_still_creates_repository_and_separate_clones_stay_separate() {
        let mut workspaces = vec![
            checkout("one", "/clones/one/.git", "/clones/one", false),
            checkout("two", "/clones/two/.git", "/clones/two", false),
        ];
        let mut repositories = Vec::new();
        let mut order = Vec::new();
        reconcile(&mut workspaces, &mut repositories, &mut order, None);

        assert_eq!(repositories.len(), 2);
        assert_ne!(repositories[0].id, repositories[1].id);
        assert_eq!(order.len(), 2);
        assert!(repositories
            .iter()
            .all(|repository| repository.checkout_workspace_ids.len() == 1));
    }

    #[test]
    fn flat_moves_project_to_repository_and_checkout_order() {
        let mut workspaces = vec![
            checkout("main", "/repos/app/.git", "/repos/app", false),
            checkout("feature-a", "/repos/app/.git", "/worktrees/a", true),
            checkout("standalone", "/repos/other/.git", "/repos/other", false),
            checkout("feature-b", "/repos/app/.git", "/worktrees/b", true),
        ];
        let mut repositories = Vec::new();
        let mut order = Vec::new();
        reconcile(&mut workspaces, &mut repositories, &mut order, None);
        let app_id = workspaces[0]
            .checkout
            .as_ref()
            .unwrap()
            .repository_id
            .clone();
        workspaces.swap(1, 3);

        project_workspace_order(&workspaces, &mut repositories, &mut order);

        let app = repositories
            .iter()
            .find(|repository| repository.id == app_id)
            .unwrap();
        assert_eq!(
            app.checkout_workspace_ids,
            vec![
                workspaces[0].id.clone(),
                workspaces[1].id.clone(),
                workspaces[3].id.clone(),
            ]
        );
        assert_eq!(order[0], SpaceRef::Repository(app_id));
    }

    #[test]
    fn reconciliation_repairs_stale_mru_and_preserves_repository_preferences() {
        let mut workspaces = vec![checkout("main", "/repos/app/.git", "/repos/app", false)];
        let mut repositories = vec![Repository {
            id: stable_repository_id(Path::new("/repos/app/.git")),
            git_common_dir: "/repos/app/.git".into(),
            label: "app".into(),
            custom_name: Some("Product".into()),
            preferred_base: Some("origin/trunk".into()),
            checkout_workspace_ids: vec!["missing".into()],
            last_focused_workspace_id: Some("missing".into()),
        }];
        let mut order = vec![SpaceRef::Repository(repositories[0].id.clone())];
        reconcile(&mut workspaces, &mut repositories, &mut order, None);

        assert_eq!(repositories[0].custom_name.as_deref(), Some("Product"));
        assert_eq!(
            repositories[0].preferred_base.as_deref(),
            Some("origin/trunk")
        );
        assert_eq!(
            repositories[0].last_focused_workspace_id.as_deref(),
            Some(workspaces[0].id.as_str())
        );
    }
}
