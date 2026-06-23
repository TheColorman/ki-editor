use crate::grid::StyleKey;
use ropey::Rope;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JjConflictLineKind {
    Marker,
    Diff,
    DiffRemoved,
    DiffAdded,
    Snapshot,
}

impl JjConflictLineKind {
    pub fn style_key(self) -> StyleKey {
        match self {
            JjConflictLineKind::Marker => StyleKey::JjConflictMarker,
            JjConflictLineKind::Diff => StyleKey::JjConflictDiff,
            JjConflictLineKind::DiffRemoved => StyleKey::HunkOld,
            JjConflictLineKind::DiffAdded => StyleKey::HunkNew,
            JjConflictLineKind::Snapshot => StyleKey::JjConflictSnapshot,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JjConflictLine {
    pub line_index: usize,
    pub kind: JjConflictLineKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JjConflictSectionKind {
    Diff,
    Snapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JjConflictSection {
    pub kind: JjConflictSectionKind,
    pub marker_line: usize,
    pub body_line_range: std::ops::Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JjConflict {
    pub line_range: std::ops::Range<usize>,
    pub sections: Vec<JjConflictSection>,
}

impl JjConflict {
    pub fn section_containing_line(&self, line_index: usize) -> Option<&JjConflictSection> {
        self.sections
            .iter()
            .find(|section| section.body_line_range.contains(&line_index))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Diff,
    Snapshot,
}

#[cfg(test)]
pub fn lines(content: &str) -> Vec<JjConflictLine> {
    lines_iter(content.lines().enumerate())
}

pub fn lines_from_rope(rope: &Rope) -> Vec<JjConflictLine> {
    lines_iter(
        rope.lines()
            .enumerate()
            .map(|(line_index, line)| (line_index, line.to_string())),
    )
}

pub fn conflicts_from_rope(rope: &Rope) -> Vec<JjConflict> {
    conflicts_iter(
        rope.lines()
            .enumerate()
            .map(|(line_index, line)| (line_index, line.to_string())),
    )
}

fn conflicts_iter(lines: impl IntoIterator<Item = (usize, impl AsRef<str>)>) -> Vec<JjConflict> {
    let mut conflicts = Vec::new();
    let mut current_conflict: Option<JjConflict> = None;
    let mut current_section: Option<JjConflictSection> = None;

    for (line_index, line) in lines {
        let line = line.as_ref().trim_end_matches(['\r', '\n']);

        if is_marker(line, '<') {
            current_conflict = Some(JjConflict {
                line_range: line_index..line_index + 1,
                sections: Vec::new(),
            });
            current_section = None;
            continue;
        }

        let Some(conflict) = current_conflict.as_mut() else {
            continue;
        };

        if is_marker(line, '>') {
            if let Some(mut section) = current_section.take() {
                section.body_line_range.end = line_index;
                conflict.sections.push(section);
            }
            conflict.line_range.end = line_index + 1;
            if let Some(conflict) = current_conflict.take() {
                conflicts.push(conflict);
            }
            continue;
        }

        let section_kind = if is_marker(line, '%') || is_marker(line, '\\') {
            Some(JjConflictSectionKind::Diff)
        } else if is_marker(line, '+') || is_marker(line, '-') {
            Some(JjConflictSectionKind::Snapshot)
        } else {
            None
        };

        if let Some(kind) = section_kind {
            if let Some(mut section) = current_section.take() {
                section.body_line_range.end = line_index;
                conflict.sections.push(section);
            }
            current_section = Some(JjConflictSection {
                kind,
                marker_line: line_index,
                body_line_range: line_index + 1..line_index + 1,
            });
        }
    }

    conflicts
}

fn lines_iter(lines: impl IntoIterator<Item = (usize, impl AsRef<str>)>) -> Vec<JjConflictLine> {
    let mut result = Vec::new();
    let mut in_conflict = false;
    let mut section = None;

    for (line_index, line) in lines {
        let line = line.as_ref().trim_end_matches(['\r', '\n']);
        if is_marker(line, '<') {
            in_conflict = true;
            section = None;
            result.push(JjConflictLine {
                line_index,
                kind: JjConflictLineKind::Marker,
            });
            continue;
        }

        if !in_conflict {
            continue;
        }

        if is_marker(line, '>') {
            in_conflict = false;
            section = None;
            result.push(JjConflictLine {
                line_index,
                kind: JjConflictLineKind::Marker,
            });
            continue;
        }

        if is_marker(line, '%') || is_marker(line, '\\') {
            section = Some(Section::Diff);
            result.push(JjConflictLine {
                line_index,
                kind: JjConflictLineKind::Marker,
            });
            continue;
        }

        if is_marker(line, '+') || is_marker(line, '-') {
            section = Some(Section::Snapshot);
            result.push(JjConflictLine {
                line_index,
                kind: JjConflictLineKind::Marker,
            });
            continue;
        }

        let Some(section) = section else {
            result.push(JjConflictLine {
                line_index,
                kind: JjConflictLineKind::Marker,
            });
            continue;
        };

        let kind = match section {
            Section::Snapshot => JjConflictLineKind::Snapshot,
            Section::Diff if line.starts_with('+') => JjConflictLineKind::DiffAdded,
            Section::Diff if line.starts_with('-') => JjConflictLineKind::DiffRemoved,
            Section::Diff => JjConflictLineKind::Diff,
        };
        result.push(JjConflictLine { line_index, kind });
    }

    result
}

fn is_marker(line: &str, marker: char) -> bool {
    line.chars().take_while(|c| c == &marker).count() >= 7
}

#[cfg(test)]
mod tests {
    use super::{lines, JjConflictLine, JjConflictLineKind};
    use crate::{
        app::Dimension,
        components::{component::RenderTitleMode, editor::Editor},
        context::Context,
        grid::Grid,
        grid::StyleKey,
        themes::Theme,
    };

    #[test]
    fn highlights_jj_diff_and_snapshot_sections() {
        let content = [
            "before",
            "<<<<<<< conflict 1 of 1",
            "%%%%%%% diff from: base",
            "\\\\\\\\\\\\\\        to: side-a",
            " context",
            "-old",
            "+new",
            "+++++++ side-b",
            "snapshot",
            ">>>>>>> conflict 1 of 1 ends",
            "after",
        ]
        .join("\n");

        assert_eq!(
            lines(&content),
            vec![
                JjConflictLine {
                    line_index: 1,
                    kind: JjConflictLineKind::Marker,
                },
                JjConflictLine {
                    line_index: 2,
                    kind: JjConflictLineKind::Marker,
                },
                JjConflictLine {
                    line_index: 3,
                    kind: JjConflictLineKind::Marker,
                },
                JjConflictLine {
                    line_index: 4,
                    kind: JjConflictLineKind::Diff,
                },
                JjConflictLine {
                    line_index: 5,
                    kind: JjConflictLineKind::DiffRemoved,
                },
                JjConflictLine {
                    line_index: 6,
                    kind: JjConflictLineKind::DiffAdded,
                },
                JjConflictLine {
                    line_index: 7,
                    kind: JjConflictLineKind::Marker,
                },
                JjConflictLine {
                    line_index: 8,
                    kind: JjConflictLineKind::Snapshot,
                },
                JjConflictLine {
                    line_index: 9,
                    kind: JjConflictLineKind::Marker,
                },
            ]
        );
    }

    #[test]
    fn supports_long_markers_and_snapshot_base_sections() {
        let content = [
            "<<<<<<<<<<<<<<< conflict 1 of 1",
            "+++++++++++++++ side-a",
            "left",
            "--------------- base",
            "base",
            ">>>>>>>>>>>>>>> conflict 1 of 1 ends",
        ]
        .join("\n");

        assert_eq!(
            lines(&content),
            vec![
                JjConflictLine {
                    line_index: 0,
                    kind: JjConflictLineKind::Marker,
                },
                JjConflictLine {
                    line_index: 1,
                    kind: JjConflictLineKind::Marker,
                },
                JjConflictLine {
                    line_index: 2,
                    kind: JjConflictLineKind::Snapshot,
                },
                JjConflictLine {
                    line_index: 3,
                    kind: JjConflictLineKind::Marker,
                },
                JjConflictLine {
                    line_index: 4,
                    kind: JjConflictLineKind::Snapshot,
                },
                JjConflictLine {
                    line_index: 5,
                    kind: JjConflictLineKind::Marker,
                },
            ]
        );
    }

    #[test]
    fn renders_jj_conflict_highlights() -> anyhow::Result<()> {
        let content = [
            "<<<<<<< conflict 1 of 1",
            "%%%%%%% diff from: base",
            "\\\\\\\\\\\\\\        to: side-a",
            " context",
            "-old",
            "+new",
            "+++++++ side-b",
            "snapshot",
            ">>>>>>> conflict 1 of 1 ends",
        ]
        .join("\n");

        let mut editor = Editor::from_text(None, &content);
        let context = Context::default();
        let grid = editor
            .get_grid_with_custom_dimension(
                &context,
                true,
                Dimension {
                    height: 20,
                    width: 80,
                },
                &None,
                &RenderTitleMode::Filename,
                true,
            )
            .grid;

        assert_grid_style(
            &grid,
            context.theme(),
            "<<<<<<<",
            StyleKey::JjConflictMarker,
        );
        assert_grid_style(&grid, context.theme(), "context", StyleKey::JjConflictDiff);
        assert_grid_style(&grid, context.theme(), "old", StyleKey::HunkOld);
        assert_grid_style(&grid, context.theme(), "new", StyleKey::HunkNew);
        assert_grid_style(
            &grid,
            context.theme(),
            "snapshot",
            StyleKey::JjConflictSnapshot,
        );

        Ok(())
    }

    #[test]
    fn renders_jj_diff_lines_after_wrapped_marker_lines() -> anyhow::Result<()> {
        let content = [
            "<<<<<<< conflict 1 of 1 with a long label that wraps",
            "%%%%%%% diff from: base with a long label that wraps",
            "\\\\\\\\\\\\\\        to: side-a with a long label that wraps",
            " context",
            "-old",
            "+new",
            "+++++++ side-b",
            "snapshot",
            ">>>>>>> conflict 1 of 1 ends",
        ]
        .join("\n");

        let mut editor = Editor::from_text(None, &content);
        let context = Context::default();
        let grid = editor
            .get_grid_with_custom_dimension(
                &context,
                true,
                Dimension {
                    height: 20,
                    width: 35,
                },
                &None,
                &RenderTitleMode::Filename,
                true,
            )
            .grid;

        assert_grid_style(&grid, context.theme(), "context", StyleKey::JjConflictDiff);
        assert_grid_style(&grid, context.theme(), "old", StyleKey::HunkOld);
        assert_grid_style(&grid, context.theme(), "new", StyleKey::HunkNew);

        Ok(())
    }

    fn assert_grid_style(grid: &Grid, theme: &Theme, search: &str, expected: StyleKey) {
        let matches = grid
            .rows
            .iter()
            .enumerate()
            .filter_map(|(line, row)| {
                let row_string = row.iter().map(|cell| cell.symbol).collect::<String>();
                Some((line, row_string.find(search)?))
            })
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "expected one match for {search:?}");
        let (line, column) = matches[0];
        let expected_style = theme.get_style(&expected);
        for column in column..column + search.len() {
            if let Some(background_color) = expected_style.background_color {
                assert_eq!(grid.rows[line][column].background_color, background_color);
            }
            if let Some(foreground_color) = expected_style.foreground_color {
                assert_eq!(grid.rows[line][column].foreground_color, foreground_color);
            }
        }
    }
}
