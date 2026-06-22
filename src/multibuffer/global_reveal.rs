use crate::components::component::Component;
use crate::components::suggestive_editor::SuggestiveEditor;
use crate::{
    app::App,
    buffer::BufferOwner,
    char_index_range::CharIndexRange,
    components::editor::{DispatchEditor, IfCurrentNotFound, Reveal},
    context::GlobalMode,
    frontend::Frontend,
    multibuffer::{Multibuffer, MultibufferFile},
    selection::SelectionMode,
};

use itertools::Itertools;
use shared::absolute_path::AbsolutePath;
use std::{cell::RefCell, rc::Rc};

use super::MultibufferRange;

pub struct GlobalReveal {
    pub reveal: Reveal,
    pub files: Vec<GlobalRevealFile>,
}

pub struct GlobalRevealFile {
    pub path: AbsolutePath,
    /// Ranges of the possible selections
    pub possible_selection_ranges: Vec<CharIndexRange>,
    pub editor: Rc<RefCell<SuggestiveEditor>>,
}
impl GlobalRevealFile {
    pub(crate) fn to_multibuffer_path(&self) -> MultibufferFile {
        MultibufferFile {
            path: self.path.clone(),
            editor: self.editor.clone(),
        }
    }
}

impl GlobalReveal {
    #[cfg(test)]
    pub fn editors(&self) -> Vec<Rc<RefCell<SuggestiveEditor>>> {
        self.files
            .iter()
            .map(|file| file.editor.clone())
            .collect_vec()
    }

    pub fn ranges(&self, current_file_path: &AbsolutePath) -> Vec<MultibufferRange> {
        self.files
            .iter()
            .flat_map(|file| {
                let binding = file.editor.borrow();
                let selection_set = &binding.editor().selection_set;
                let primary_selection_range = selection_set.primary_selection().range;
                let has_primary_range = file
                    .possible_selection_ranges
                    .contains(&primary_selection_range);
                file.possible_selection_ranges
                    .iter()
                    .copied()
                    .unique()
                    .enumerate()
                    .map(|(index, range)| {
                        let is_current_file = &file.path == current_file_path;
                        let is_primary = is_current_file
                            && (range == primary_selection_range
                                || (!has_primary_range && index == 0));
                        MultibufferRange {
                            path: file.path.clone(),
                            range,
                            is_primary,
                        }
                    })
                    .collect_vec()
            })
            .collect_vec()
    }

    pub fn focused_file_index(&self, current_file_path: &AbsolutePath) -> Option<usize> {
        self.files
            .iter()
            .position(|file| &file.path == current_file_path)
    }
}
impl<T: Frontend> App<T> {
    pub fn toggle_reveal_marks(&mut self) -> anyhow::Result<()> {
        if matches!(
            self.multibuffer,
            Some(Multibuffer::GlobalReveal(GlobalReveal {
                reveal: Reveal::Mark,
                ..
            }))
        ) {
            self.multibuffer = None;
            return Ok(());
        }

        self.activate_global_reveal_marks()
    }

    pub fn toggle_reveal_selections(&mut self) -> anyhow::Result<()> {
        if let Some(Multibuffer::GlobalReveal(_)) = self.multibuffer {
            self.multibuffer = None;
            Ok(())
        } else if self.context.mode() == Some(GlobalMode::QuickfixListItem) {
            self.activate_global_reveal_selections()
        } else {
            self.handle_dispatch_editor(DispatchEditor::ToggleReveal(Reveal::CurrentSelectionMode))
        }
    }

    fn activate_global_reveal_selections(&mut self) -> anyhow::Result<()> {
        let grouped_ranges = self
            .quickfix_list()
            .items()
            .iter()
            .sorted_by_key(|item| item.location().path.clone())
            .chunk_by(|item| item.location().path.clone())
            .into_iter()
            .map(|(path, items)| {
                (
                    path,
                    items
                        .into_iter()
                        .map(|item| item.location().range)
                        .collect_vec(),
                )
            })
            .collect_vec();

        let files = grouped_ranges
            .into_iter()
            .map(|(path, ranges)| -> anyhow::Result<_> {
                let editor = self.open_file(&path, BufferOwner::User, true, false)?;

                self.handle_dispatch_editor_custom(
                    DispatchEditor::SetSelectionMode(
                        IfCurrentNotFound::LookForward,
                        SelectionMode::LocalQuickfix {
                            title: self.quickfix_list().title().to_string(),
                        },
                    ),
                    editor.clone(),
                )?;

                Ok(GlobalRevealFile {
                    path,
                    editor,
                    possible_selection_ranges: ranges,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        if !files.is_empty() {
            self.multibuffer = Some(Multibuffer::GlobalReveal(GlobalReveal {
                reveal: Reveal::CurrentSelectionMode,
                files,
            }));

            // Close the quickfix list
            self.layout.remain_only_current_component();
        }

        // This is a hack: we need to reset the global mode because it is cleared when `self.open_file` is invoked
        self.context.set_mode(Some(GlobalMode::QuickfixListItem));

        Ok(())
    }

    fn activate_global_reveal_marks(&mut self) -> anyhow::Result<()> {
        let current_component = self.current_component();
        let current_file = {
            let current_component = current_component.borrow();
            current_component.path().map(|path| {
                (
                    path,
                    current_component
                        .editor()
                        .selection_set
                        .primary_selection()
                        .range,
                )
            })
        };

        let persisted_marks = self
            .context
            .marks()
            .iter()
            .filter(|(_, marks)| !marks.is_empty())
            .sorted_by_key(|(path, _)| (*path).clone())
            .map(|(path, marks)| (path.clone(), marks.clone()))
            .collect_vec();

        let grouped_marks = persisted_marks
            .into_iter()
            .map(|(path, ranges)| -> anyhow::Result<Option<_>> {
                let editor = self.open_file(&path, BufferOwner::User, true, false)?;
                let valid_ranges = {
                    let editor = editor.borrow();
                    let buffer = editor.editor().buffer();
                    ranges
                        .into_iter()
                        .unique()
                        .filter(|range| buffer.char_index_range_to_byte_range(*range).is_ok())
                        .collect_vec()
                };

                self.context.set_marks(path.clone(), valid_ranges.clone());

                Ok((!valid_ranges.is_empty()).then_some((path, editor, valid_ranges)))
            })
            .collect::<anyhow::Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect_vec();

        if grouped_marks.is_empty() {
            self.handle_dispatch_editor(DispatchEditor::ToggleReveal(Reveal::Mark))?;
            return Ok(());
        }

        let current_file_has_marks = current_file.as_ref().is_some_and(|(current_path, _)| {
            grouped_marks
                .iter()
                .any(|(path, _, _)| path == current_path)
        });

        let current_file_entry = if current_file_has_marks {
            None
        } else {
            current_file
                .as_ref()
                .map(|(current_path, anchor_range)| {
                    self.open_file(current_path, BufferOwner::User, true, false)
                        .map(|editor| (current_path.clone(), editor, vec![*anchor_range]))
                })
                .transpose()?
        };

        let grouped_ranges = grouped_marks
            .into_iter()
            .chain(current_file_entry)
            .map(|(path, editor, mut ranges)| {
                if let Some((current_path, anchor_range)) = &current_file {
                    if path == *current_path {
                        ranges.push(*anchor_range);
                    }
                }

                (path, editor, ranges)
            })
            .collect_vec();

        let files = grouped_ranges
            .into_iter()
            .map(|(path, editor, ranges)| {
                Ok(GlobalRevealFile {
                    path,
                    editor,
                    possible_selection_ranges: ranges.into_iter().unique().collect_vec(),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        if !files.is_empty() {
            self.multibuffer = Some(Multibuffer::GlobalReveal(GlobalReveal {
                reveal: Reveal::Mark,
                files,
            }));
            self.layout.remain_only_current_component();
        }

        Ok(())
    }
}
