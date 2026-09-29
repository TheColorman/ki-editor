use super::*;

const POLICY: WorkspaceRootPolicy = WorkspaceRootPolicy {
    marker_groups: &[
        &[RootMarker::Paths(&["workspace.marker"])],
        &[RootMarker::Paths(&["project.marker"])],
        &[RootMarker::Paths(&["package.marker"])],
    ],
    fallback: RootFallback::Boundary,
    canonicalize: false,
};

#[test]
fn priority_beats_proximity_and_falls_back_through_all_groups() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    let package = project.join("package");
    let source = package.join("src");
    std::fs::create_dir_all(&source)?;
    std::fs::write(temp.path().join("workspace.marker"), "")?;
    std::fs::write(project.join("project.marker"), "")?;
    std::fs::write(package.join("package.marker"), "")?;

    assert_eq!(POLICY.resolve(&source, temp.path()), temp.path());
    std::fs::remove_file(temp.path().join("workspace.marker"))?;
    assert_eq!(POLICY.resolve(&source, temp.path()), project);
    std::fs::remove_file(project.join("project.marker"))?;
    assert_eq!(POLICY.resolve(&source, temp.path()), package);
    std::fs::remove_file(package.join("package.marker"))?;
    assert_eq!(POLICY.resolve(&source, temp.path()), temp.path());
    Ok(())
}

#[test]
fn nearest_match_wins_within_a_group_even_across_marker_kinds() -> anyhow::Result<()> {
    let policy = WorkspaceRootPolicy {
        marker_groups: &[&[
            RootMarker::Paths(&["workspace.marker"]),
            RootMarker::Directories(&[".project"]),
        ]],
        ..POLICY
    };
    let temp = tempfile::tempdir()?;
    let nested = temp.path().join("nested");
    let source = nested.join("src");
    std::fs::create_dir_all(&source)?;
    std::fs::create_dir(nested.join(".project"))?;
    std::fs::write(temp.path().join("workspace.marker"), "")?;
    assert_eq!(policy.resolve(&source, temp.path()), nested);
    Ok(())
}

#[test]
fn boundary_is_included_but_its_ancestors_are_not_searched() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let boundary = temp.path().join("workspace");
    let source = boundary.join("project/src");
    std::fs::create_dir_all(&source)?;
    std::fs::write(temp.path().join("workspace.marker"), "")?;
    std::fs::write(boundary.join("project.marker"), "")?;
    assert_eq!(POLICY.resolve(&source, &boundary), boundary);
    assert_eq!(POLICY.resolve(&boundary, &boundary), boundary);

    std::fs::remove_file(boundary.join("project.marker"))?;
    let policy = WorkspaceRootPolicy {
        fallback: RootFallback::FileParent,
        ..POLICY
    };
    assert_eq!(policy.resolve(&source, &boundary), source);
    Ok(())
}

#[test]
fn files_outside_the_boundary_search_their_own_ancestors() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let boundary = temp.path().join("cwd");
    let project = temp.path().join("other-project");
    let source = project.join("src");
    std::fs::create_dir_all(&source)?;
    std::fs::create_dir(&boundary)?;
    std::fs::write(project.join("project.marker"), "")?;
    std::fs::write(boundary.join("workspace.marker"), "")?;
    assert_eq!(POLICY.resolve(&source, &boundary), project);

    std::fs::remove_file(project.join("project.marker"))?;
    assert_eq!(POLICY.resolve(&source, &boundary), boundary);
    let policy = WorkspaceRootPolicy {
        fallback: RootFallback::FileParent,
        ..POLICY
    };
    assert_eq!(policy.resolve(&source, &boundary), source);
    Ok(())
}

#[test]
fn directory_markers_do_not_match_regular_files() -> anyhow::Result<()> {
    let policy = WorkspaceRootPolicy {
        marker_groups: &[&[RootMarker::Directories(&[".project"])]],
        ..POLICY
    };
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("src");
    std::fs::create_dir(&source)?;
    std::fs::create_dir(temp.path().join(".project"))?;
    std::fs::write(source.join(".project"), "")?;
    assert_eq!(policy.resolve(&source, temp.path()), temp.path());

    let policy = WorkspaceRootPolicy {
        marker_groups: &[&[RootMarker::Paths(&[".project"])]],
        ..policy
    };
    assert_eq!(policy.resolve(&source, temp.path()), source);
    assert_eq!(policy.resolve(temp.path(), temp.path()), temp.path());
    Ok(())
}

#[test]
fn extension_groups_use_case_insensitive_matching_and_priority() -> anyhow::Result<()> {
    let policy = WorkspaceRootPolicy {
        marker_groups: &[
            &[RootMarker::Extensions(&["workspace"])],
            &[RootMarker::Extensions(&["project"])],
        ],
        ..POLICY
    };
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    std::fs::write(temp.path().join("App.WORKSPACE"), "")?;
    std::fs::write(project.join("App.ProJect"), "")?;
    assert_eq!(policy.resolve(&project, temp.path()), temp.path());
    std::fs::remove_file(temp.path().join("App.WORKSPACE"))?;
    assert_eq!(policy.resolve(&project, temp.path()), project);
    Ok(())
}

#[test]
fn extension_groups_share_one_lazy_directory_listing() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("App.project"), "")?;
    let candidate = RootCandidate::new(temp.path());
    assert!(!candidate.matches(&RootMarker::Paths(&["absent"])));
    assert!(candidate.extensions.get().is_none());
    assert!(candidate.matches(&RootMarker::Extensions(&["project"])));

    // Subsequent groups use the same listing rather than issuing another read.
    std::fs::write(temp.path().join("App.workspace"), "")?;
    assert!(!candidate.matches(&RootMarker::Extensions(&["workspace"])));
    assert!(RootCandidate::new(temp.path()).matches(&RootMarker::Extensions(&["workspace"])));
    Ok(())
}

#[test]
fn missing_paths_and_failed_canonicalization_preserve_the_fallback() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let missing = temp.path().join("not-created/src");
    let policy = WorkspaceRootPolicy {
        marker_groups: &[&[RootMarker::Extensions(&["workspace"])]],
        fallback: RootFallback::FileParent,
        canonicalize: true,
    };
    assert_eq!(policy.resolve(&missing, temp.path()), missing);
    Ok(())
}

#[cfg(unix)]
#[test]
fn canonicalization_is_opt_in_and_applies_to_matches_and_fallbacks() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    let link = temp.path().join("linked-project");
    std::fs::create_dir_all(project.join("src"))?;
    std::fs::write(project.join("workspace.marker"), "")?;
    std::os::unix::fs::symlink(&project, &link)?;
    let source = link.join("src");
    assert_eq!(POLICY.resolve(&source, temp.path()), link);
    let policy = WorkspaceRootPolicy {
        canonicalize: true,
        ..POLICY
    };
    assert_eq!(
        policy.resolve(&source, temp.path()),
        project.canonicalize()?
    );

    let policy = WorkspaceRootPolicy {
        marker_groups: &[],
        fallback: RootFallback::FileParent,
        ..policy
    };
    assert_eq!(
        policy.resolve(&source, temp.path()),
        project.join("src").canonicalize()?
    );
    Ok(())
}
