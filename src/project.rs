//! Grouping sessions by project.
//!
//! A session belongs to a project when its working directory is the project
//! folder, a folder inside it, or one of its git worktrees. Worktrees are
//! recognised by path, in the two common layouts:
//!
//! - `<project>__worktrees/<branch>` (workmux default, sibling of the checkout)
//! - `<project>/.worktrees/<branch>`

const WORKTREE_MARKERS: &[&str] = &["__worktrees/", "/.worktrees/"];

/// The project folder a working directory belongs to, with any worktree part
/// removed. `/p/xenia__worktrees/fix-bug/src` becomes `/p/xenia`.
pub fn project_root(cwd: &str) -> String {
    let with_slash = format!("{}/", cwd.trim_end_matches('/'));
    let root = WORKTREE_MARKERS
        .iter()
        .filter_map(|marker| with_slash.find(marker))
        .min()
        .map(|pos| &with_slash[..pos])
        .unwrap_or(&with_slash);
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        "/".to_string()
    } else {
        root.to_string()
    }
}

/// The worktree folder name when a working directory is inside a git
/// worktree. `/p/xenia__worktrees/fix-bug/src` gives `fix-bug`.
pub fn worktree_name(cwd: &str) -> Option<String> {
    let (pos, marker) = WORKTREE_MARKERS
        .iter()
        .filter_map(|marker| cwd.find(marker).map(|pos| (pos, marker)))
        .min_by_key(|(pos, _)| *pos)?;
    let rest = &cwd[pos + marker.len()..];
    let name = rest.split('/').next().unwrap_or("");
    (!name.is_empty()).then(|| name.to_string())
}

/// Last folder name of the project a working directory belongs to
pub fn project_name(cwd: &str) -> String {
    let root = project_root(cwd);
    root.rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(&root)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worktree_name() {
        assert_eq!(
            worktree_name("/p/xenia__worktrees/fix-bug/src").as_deref(),
            Some("fix-bug")
        );
        assert_eq!(worktree_name("/p/xenia/.worktrees/fix-bug").as_deref(), Some("fix-bug"));
        assert_eq!(worktree_name("/p/xenia/src"), None);
        assert_eq!(worktree_name("/p/xenia__worktrees"), None);
    }

    #[test]
    fn test_project_name() {
        assert_eq!(project_name("/p/xenia__worktrees/fix-bug/src"), "xenia");
        assert_eq!(project_name("/p/xenia"), "xenia");
        assert_eq!(project_name("/"), "/");
    }

    #[test]
    fn test_plain_folder_is_its_own_root() {
        assert_eq!(
            project_root("/Users/me/Programming/xenia"),
            "/Users/me/Programming/xenia"
        );
    }

    #[test]
    fn test_trailing_slash_removed() {
        assert_eq!(
            project_root("/Users/me/Programming/xenia/"),
            "/Users/me/Programming/xenia"
        );
    }

    #[test]
    fn test_sibling_worktree_maps_to_project() {
        assert_eq!(
            project_root("/Users/me/Programming/xenia__worktrees/fix-bug"),
            "/Users/me/Programming/xenia"
        );
    }

    #[test]
    fn test_subfolder_of_sibling_worktree_maps_to_project() {
        assert_eq!(
            project_root("/Users/me/Programming/xenia__worktrees/fix-bug/frontend"),
            "/Users/me/Programming/xenia"
        );
    }

    #[test]
    fn test_nested_worktree_maps_to_project() {
        assert_eq!(
            project_root("/Users/me/Programming/xenia/.worktrees/fix-bug"),
            "/Users/me/Programming/xenia"
        );
    }

    #[test]
    fn test_similar_names_are_different_projects() {
        assert_eq!(
            project_root("/Users/me/Programming/xenia2"),
            "/Users/me/Programming/xenia2"
        );
        assert_eq!(
            project_root("/Users/me/Programming/xenia-node24"),
            "/Users/me/Programming/xenia-node24"
        );
    }

    #[test]
    fn test_worktrees_folder_itself_maps_to_project() {
        assert_eq!(
            project_root("/Users/me/Programming/xenia__worktrees"),
            "/Users/me/Programming/xenia"
        );
    }

    #[test]
    fn test_root_path() {
        assert_eq!(project_root("/"), "/");
    }
}
