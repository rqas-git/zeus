//! Workspace-level Git operations owned by the backend.

use std::path::Path;
use std::process::Command;

use anyhow::Context;
use anyhow::Result;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkspaceSnapshot {
    pub(crate) workspace_root: String,
    pub(crate) branch: Option<String>,
    pub(crate) branches: Vec<String>,
    pub(crate) git: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BranchSwitchResult {
    pub(crate) previous_branch: Option<String>,
    pub(crate) branch: String,
    pub(crate) stashed_changes: bool,
    pub(crate) workspace: WorkspaceSnapshot,
}

pub(crate) fn snapshot(root: &Path) -> Result<WorkspaceSnapshot> {
    if !is_git_workspace(root) {
        return Ok(WorkspaceSnapshot {
            workspace_root: root.display().to_string(),
            branch: None,
            branches: Vec::new(),
            git: false,
        });
    }

    let branch = current_branch(root)?;
    let mut branches = local_branches(root)?;
    if let Some(branch) = &branch {
        if !branches.iter().any(|candidate| candidate == branch) {
            branches.push(branch.clone());
        }
    }

    Ok(WorkspaceSnapshot {
        workspace_root: root.display().to_string(),
        branch,
        branches,
        git: true,
    })
}

pub(crate) fn switch_branch(root: &Path, branch: &str) -> Result<BranchSwitchResult> {
    anyhow::ensure!(is_git_workspace(root), "workspace is not a Git repository");
    let target = branch.trim();
    anyhow::ensure!(!target.is_empty(), "branch cannot be empty");

    let branches = local_branches(root)?;
    anyhow::ensure!(
        branches.iter().any(|candidate| candidate == target),
        "Git branch {target} is not available in this repository"
    );

    let previous_branch = current_branch(root)?;
    if previous_branch.as_deref() == Some(target) {
        return Ok(BranchSwitchResult {
            previous_branch,
            branch: target.to_string(),
            stashed_changes: false,
            workspace: snapshot(root)?,
        });
    }

    let stashed_changes = has_changes(root)?;
    if stashed_changes {
        let previous = previous_branch.as_deref().unwrap_or("detached");
        let message = format!("zeus: auto-stash before switching from {previous} to {target}");
        run_git(
            root,
            &["stash", "push", "--include-untracked", "-m", &message],
        )?;
    }

    run_git(root, &["switch", target])?;
    let workspace = snapshot(root)?;
    let branch = workspace
        .branch
        .clone()
        .unwrap_or_else(|| target.to_string());

    Ok(BranchSwitchResult {
        previous_branch,
        branch,
        stashed_changes,
        workspace,
    })
}

fn is_git_workspace(root: &Path) -> bool {
    run_git_optional(root, &["rev-parse", "--is-inside-work-tree"])
        .is_some_and(|output| output == "true")
}

fn current_branch(root: &Path) -> Result<Option<String>> {
    let branch = run_git(root, &["branch", "--show-current"])?;
    if !branch.is_empty() {
        return Ok(Some(branch));
    }

    let commit = run_git_optional(root, &["rev-parse", "--short", "HEAD"]);
    Ok(commit
        .filter(|commit| !commit.is_empty())
        .map(|commit| format!("detached@{commit}")))
}

fn local_branches(root: &Path) -> Result<Vec<String>> {
    let output = run_git(
        root,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )?;
    Ok(output
        .split('\n')
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
        .map(str::to_string)
        .collect())
}

fn has_changes(root: &Path) -> Result<bool> {
    Ok(!run_git(root, &["status", "--porcelain"])?.is_empty())
}

fn run_git_optional(root: &Path, args: &[&str]) -> Option<String> {
    run_git(root, args).ok()
}

fn run_git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let details = [stdout, stderr]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let suffix = if details.is_empty() {
            String::new()
        } else {
            format!(": {details}")
        };
        anyhow::bail!(
            "git {} failed with status {}{suffix}",
            args.join(" "),
            output.status
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
