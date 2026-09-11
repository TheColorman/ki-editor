use itertools::Itertools;

use crate::{
    components::editor::Direction,
    selection_mode::{syntax_token::SyntaxToken, ApplyMovementResult},
};

use super::{ByteRange, IterBasedSelectionMode, TopNode};

pub struct SyntaxNode {
    pub coarse: bool,
}

impl IterBasedSelectionMode for SyntaxNode {
    fn iter_revealed<'a>(
        &'a self,
        params: &super::SelectionModeParams<'a>,
    ) -> anyhow::Result<Box<dyn Iterator<Item = super::ByteRange> + 'a>> {
        let buffer = params.buffer;
        let current_selection = params.current_selection;
        let layer = buffer
            .syntax_tree_layer_for_selection(current_selection)?
            .ok_or(anyhow::anyhow!(
                "SyntaxNode::iter.get_current_node: Cannot find Treesitter language"
            ))?;
        let node = buffer
            .get_current_node_in_tree(&layer.tree, current_selection, false)?
            .ok_or(anyhow::anyhow!(
                "SyntaxNode::iter.get_current_node: Cannot find Treesitter language"
            ))?;
        let Some(node) = node.parent() else {
            return Ok(Box::new(std::iter::empty()));
        };
        let mut cursor = layer.tree.walk();
        let vector = if self.coarse {
            node.named_children(&mut cursor).collect_vec()
        } else {
            node.children(&mut cursor).collect_vec()
        };
        let ranges = vector
            .into_iter()
            .map(|node| ByteRange::new(buffer.node_selection_range(node)))
            .collect_vec();
        Ok(Box::new(ranges.into_iter()))
    }
    fn iter<'a>(
        &'a self,
        params: &super::SelectionModeParams<'a>,
    ) -> anyhow::Result<Box<dyn Iterator<Item = super::ByteRange> + 'a>> {
        if self.coarse {
            TopNode.iter(params)
        } else {
            SyntaxToken.iter(params)
        }
    }
    fn expand(
        &self,
        params: &super::SelectionModeParams,
    ) -> anyhow::Result<Option<ApplyMovementResult>> {
        self.select_vertical(params, true)
    }
    fn down(
        &self,
        params: &super::SelectionModeParams,
        _: Option<usize>,
    ) -> anyhow::Result<Option<ApplyMovementResult>> {
        self.select_vertical(params, false)
    }

    fn up(
        &self,
        params: &super::SelectionModeParams,
        _: Option<usize>,
    ) -> anyhow::Result<Option<ApplyMovementResult>> {
        self.select_vertical(params, true)
    }

    fn left(
        &self,
        params: &super::SelectionModeParams,
    ) -> anyhow::Result<Option<crate::selection::Selection>> {
        self.navigate_sibling_nodes(params, &Direction::Start, true)
    }

    fn right(
        &self,
        params: &super::SelectionModeParams,
    ) -> anyhow::Result<Option<crate::selection::Selection>> {
        self.navigate_sibling_nodes(params, &Direction::End, true)
    }

    fn previous(
        &self,
        params: &super::SelectionModeParams,
    ) -> anyhow::Result<Option<crate::selection::Selection>> {
        self.navigate_sibling_nodes(params, &Direction::Start, false)
    }

    fn next(
        &self,
        params: &super::SelectionModeParams,
    ) -> anyhow::Result<Option<crate::selection::Selection>> {
        self.navigate_sibling_nodes(params, &Direction::End, false)
    }

    fn all_meaningful_selections<'a>(
        &'a self,
        params: &super::SelectionModeParams<'a>,
    ) -> anyhow::Result<Box<dyn Iterator<Item = ByteRange> + 'a>> {
        let buffer = params.buffer;
        let current_selection = params.current_selection;
        let layer = buffer
            .syntax_tree_layer_for_selection(current_selection)?
            .ok_or(anyhow::anyhow!(
                "SyntaxNode::iter: Cannot find Treesitter language"
            ))?;
        let node = buffer
            .get_current_node_in_tree(&layer.tree, current_selection, false)?
            .ok_or(anyhow::anyhow!(
                "SyntaxNode::iter: Cannot find Treesitter language"
            ))?;

        if let Some(parent) = node.parent() {
            let ranges = {
                (0..parent.named_child_count())
                    .filter_map(move |i| parent.named_child(i))
                    .map(|node| ByteRange::new(buffer.node_selection_range(node)))
                    .collect_vec()
            };
            Ok(Box::new(ranges.into_iter()))
        } else {
            Ok(Box::new(std::iter::empty()))
        }
    }

    #[cfg(test)]
    fn all_selections<'a>(
        &'a self,
        params: &super::SelectionModeParams<'a>,
    ) -> anyhow::Result<Box<dyn Iterator<Item = ByteRange> + 'a>> {
        let buffer = params.buffer;
        let current_selection = params.current_selection;
        let layer = buffer
            .syntax_tree_layer_for_selection(current_selection)?
            .ok_or(anyhow::anyhow!(
                "SyntaxNode::iter: Cannot find Treesitter language"
            ))?;
        let node = buffer
            .get_current_node_in_tree(&layer.tree, current_selection, false)?
            .ok_or(anyhow::anyhow!(
                "SyntaxNode::iter: Cannot find Treesitter language"
            ))?;

        if let Some(parent) = node.parent() {
            let ranges = {
                (0..parent.child_count())
                    .filter_map(move |i| parent.child(i))
                    .map(|node| ByteRange::new(buffer.node_selection_range(node)))
                    .collect_vec()
            };
            Ok(Box::new(ranges.into_iter()))
        } else {
            Ok(Box::new(std::iter::empty()))
        }
    }
}

impl SyntaxNode {
    fn navigate_sibling_nodes(
        &self,
        params: &super::SelectionModeParams,
        direction: &Direction,
        named: bool,
    ) -> anyhow::Result<Option<crate::selection::Selection>> {
        let buffer = params.buffer;
        let current_selection = params.current_selection;
        let layer = buffer
            .syntax_tree_layer_for_selection(current_selection)?
            .ok_or(anyhow::anyhow!(
                "SyntaxNode::iter: Cannot find Treesitter language"
            ))?;
        let node = buffer
            .get_current_node_in_tree(&layer.tree, current_selection, false)?
            .ok_or(anyhow::anyhow!(
                "SyntaxNode::iter: Cannot find Treesitter language"
            ))?;
        let node = match (named, direction) {
            (true, Direction::Start) => node.prev_named_sibling(),
            (true, Direction::End) => node.next_named_sibling(),
            (false, Direction::Start) => node.prev_sibling(),
            (false, Direction::End) => node.next_sibling(),
        };
        Ok(node.and_then(|node| {
            ByteRange::new(buffer.node_selection_range(node))
                .to_selection(params.buffer, params.current_selection)
                .ok()
        }))
    }
    pub fn select_vertical(
        &self,
        params: &super::SelectionModeParams,
        go_up: bool,
    ) -> anyhow::Result<Option<ApplyMovementResult>> {
        let Some(layer) = params
            .buffer
            .syntax_tree_layer_for_selection(params.current_selection)?
        else {
            return Ok(None);
        };
        let Some(mut node) =
            params
                .buffer
                .get_current_node_in_tree(&layer.tree, params.current_selection, false)?
        else {
            return Ok(None);
        };
        let selection_end = params
            .buffer
            .char_to_byte(params.current_selection.range().end)?;
        while let Some(some_node) = get_node(node, go_up, self.coarse) {
            // This is necessary because sometimes the parent node can have the same range as
            // the current node
            let range = params.buffer.node_selection_range(some_node);
            if range != params.buffer.node_selection_range(node)
                && (!go_up || range.end >= selection_end)
            {
                return Ok(Some(ApplyMovementResult::from_selection(
                    ByteRange::new(range).to_selection(params.buffer, params.current_selection)?,
                )));
            }
            node = some_node;
        }
        if go_up && layer.is_injected {
            if let Some(host_layer) = params.buffer.host_syntax_tree_layer() {
                if let Some(host_node) = params.buffer.get_current_node_in_tree(
                    &host_layer.tree,
                    params.current_selection,
                    false,
                )? {
                    if let Some(parent) = host_node.parent() {
                        return Ok(Some(ApplyMovementResult::from_selection(
                            ByteRange::new(params.buffer.node_selection_range(parent))
                                .to_selection(params.buffer, params.current_selection)?,
                        )));
                    }
                }
            }
        }
        Ok(None)
    }
}

pub fn get_node(node: tree_sitter::Node, go_up: bool, coarse: bool) -> Option<tree_sitter::Node> {
    match (go_up, coarse) {
        (true, _) => node.parent(),
        (false, true) => node.named_child(0),
        (false, false) => node.child(0),
    }
}

#[cfg(test)]
mod test_syntax_node {
    use crate::buffer::BufferOwner;
    use crate::selection::SelectionMode;
    use crate::selection_mode::GetGapMovement;
    use crate::test_app::*;
    use crate::{
        buffer::Buffer,
        components::editor::IfCurrentNotFound,
        selection::{CharIndex, Selection},
        selection_mode::SelectionModeParams,
    };

    use super::*;

    use serial_test::serial;

    fn vue_buffer(source: &str) -> Buffer {
        let language = crate::config::from_extension("vue").unwrap();
        let mut buffer = Buffer::new(language.tree_sitter_language(), source);
        buffer.set_language(language).unwrap();
        buffer
    }

    #[test]
    fn yaml_trailing_comments() -> anyhow::Result<()> {
        let language = crate::config::from_extension("yaml").unwrap();
        for (value, suffix) in [
            ("enabled: true", "\n  # following section\n"),
            ("enabled: true # inline", "\n  # following section\n"),
            (
                "names:\n      - default # required\n      - shard2",
                "\n\n  # Praefect\n  # URL\n",
            ),
            (
                "enabled: true\n    internal:\n      names:\n        - default # required\n        - shard2",
                "\n\n  # Praefect is the clustered version of Gitaly\n  # See:\n  # https://gitlab.com/groups/gitlab-org/-/work_items/6127\n",
            ),
            (
                "names:\n      - default\n      # internal\n      - shard2 # inline",
                "\n      # closing note\n",
            ),
            ("name: \"# not a comment\"", "\n  # following\n"),
            ("name: |\n      # scalar content", "\n  # following\n"),
            ("name: >-\n      # scalar content", "\n  # following\n"),
            (
                "enabled: true\n# less indented\n    internal: false",
                "\n  # following\n",
            ),
            ("name: \"\u{e9}\" # inline", "\n  # following\n"),
        ] {
            for newline in ["\n", "\r\n"] {
                let expected = format!("gitaly:\n    {value}").replace('\n', newline);
                let source = format!(
                    "root:\n  first: true\n  {expected}{suffix}  praefect:\n    enabled: false\n"
                )
                .replace("\r\n", "\n")
                .replace('\n', newline);
                let buffer = Buffer::new(language.tree_sitter_language(), &source);
                assert!(!buffer.tree().unwrap().root_node().has_error(), "{source}");
                let start = buffer.byte_to_char(source.find("gitaly").unwrap())?;
                let selection = Selection::new((start..start).into());
                let params = SelectionModeParams {
                    buffer: &buffer,
                    current_selection: &selection,
                    cursor_direction: &Direction::Start,
                };
                let selected = super::SyntaxNode { coarse: true }
                    .current(&params, IfCurrentNotFound::LookForward)?
                    .unwrap();
                assert_eq!(
                    buffer.slice(&selected.range())?.to_string(),
                    expected,
                    "{source}"
                );
                let tree = buffer.tree().unwrap();
                let node = buffer
                    .get_current_node_in_tree(tree, &selected, false)?
                    .unwrap();
                assert_eq!(node.kind(), "block_mapping_pair");
                assert_eq!(
                    buffer.node_selection_range(node),
                    source.find("gitaly").unwrap()..source.find("gitaly").unwrap() + expected.len()
                );
                assert!(node.end_byte() > buffer.node_selection_range(node).end);
            }
        }
        Ok(())
    }

    #[test]
    fn yaml_trimmed_node_navigation() -> anyhow::Result<()> {
        let source = "root:\n  first: true\n  gitaly:\n    names:\n      - default\n      - shard2\n\n  # Praefect\n  # URL\n  praefect:\n    enabled: false\n";
        let language = crate::config::from_extension("yaml").unwrap();
        let buffer = Buffer::new(language.tree_sitter_language(), source);
        let mode = super::SyntaxNode { coarse: true };
        let gitaly_start = source.find("gitaly").unwrap();
        let selection = Selection::new((CharIndex(gitaly_start)..CharIndex(gitaly_start)).into());
        let params = |selection| SelectionModeParams {
            buffer: &buffer,
            current_selection: selection,
            cursor_direction: &Direction::Start,
        };
        let selected = mode
            .current(&params(&selection), IfCurrentNotFound::LookForward)?
            .unwrap();
        let next = mode.right(&params(&selected))?.unwrap();
        assert_eq!(
            buffer.slice(&next.range())?.to_string(),
            "praefect:\n    enabled: false\n"
        );
        assert_eq!(
            mode.left(&params(&next))?.unwrap().range(),
            selected.range()
        );
        let child = mode.down(&params(&selected), None)?.unwrap().selection;
        assert_eq!(buffer.slice(&child.range())?.to_string(), "gitaly");
        assert_eq!(
            mode.expand(&params(&child))?.unwrap().selection.range(),
            selected.range()
        );
        let parent = mode.expand(&params(&selected))?.unwrap().selection;
        assert!(parent.range().end > selected.range().end);
        let ranges = mode
            .all_meaningful_selections(&params(&selected))?
            .collect_vec();
        assert!(ranges.contains(&ByteRange::new(
            gitaly_start..source.find("shard2").unwrap() + 6
        )));

        let start = source.find("# Praefect").unwrap();
        let comment = Selection::new((CharIndex(start)..CharIndex(start + 10)).into());
        let node = buffer
            .get_current_node_in_tree(buffer.tree().unwrap(), &comment, false)?
            .unwrap();
        assert_eq!(node.kind(), "comment");
        assert_eq!(buffer.node_selection_range(node), node.byte_range());
        if let Some(parent) = mode.expand(&params(&comment))? {
            assert!(parent.selection.range().end >= comment.range().end);
        }
        Ok(())
    }

    #[test]
    fn yaml_trailing_comments_at_eof() -> anyhow::Result<()> {
        let language = crate::config::from_extension("yaml").unwrap();
        let source = "gitaly:\n  enabled: true\n# trailing";
        let buffer = Buffer::new(language.tree_sitter_language(), source);
        let selection = Selection::default();
        let params = SelectionModeParams {
            buffer: &buffer,
            current_selection: &selection,
            cursor_direction: &Direction::Start,
        };
        let selected = super::SyntaxNode { coarse: true }
            .current(&params, IfCurrentNotFound::LookForward)?
            .unwrap();
        assert_eq!(
            buffer.slice(&selected.range())?.to_string(),
            "gitaly:\n  enabled: true"
        );
        Ok(())
    }

    #[serial]
    #[test]
    fn yaml_copy_delete_leaves_following_comments() -> anyhow::Result<()> {
        execute_test(|s| {
            let path = s.new_path("selection.yaml");
            std::fs::write(&path, "").unwrap();
            Box::new([
                App(OpenFile {
                    path: path.try_into().unwrap(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                Editor(SetContent(
                    "first: true\ngitaly:\n  names:\n    - shard2\n\n# Praefect\npraefect: false"
                        .to_string(),
                )),
                Editor(MatchLiteral("gitaly".to_string())),
                Editor(SetSelectionMode(
                    IfCurrentNotFound::LookForward,
                    SelectionMode::SyntaxNode,
                )),
                Expect(CurrentSelectedTexts(&["gitaly:\n  names:\n    - shard2"])),
                Editor(Copy),
                Editor(DeleteOne),
                Expect(CurrentComponentContent(
                    "first: true\n\n\n# Praefect\npraefect: false",
                )),
                Editor(MatchLiteral("praefect: false".to_string())),
                Editor(ReplaceWithCopiedText { cut: false }),
                Expect(CurrentComponentContent(
                    "first: true\n\n\n# Praefect\ngitaly:\n  names:\n    - shard2",
                )),
            ])
        })
    }

    fn select_current_syntax_node_text(source: &str, cursor_text: &str) -> String {
        let buffer = vue_buffer(source);
        let start = source.find(cursor_text).unwrap();
        let selection =
            Selection::default().set_range((CharIndex(start)..CharIndex(start + 1)).into());
        let selected = super::SyntaxNode { coarse: true }
            .current(
                &SelectionModeParams {
                    buffer: &buffer,
                    current_selection: &selection,
                    cursor_direction: &crate::components::editor::Direction::Start,
                },
                IfCurrentNotFound::LookForward,
            )
            .unwrap()
            .unwrap();

        buffer.slice(&selected.range()).unwrap().to_string()
    }

    #[test]
    fn case_1() {
        let buffer = Buffer::new(
            Some(tree_sitter_rust::LANGUAGE.into()),
            "fn main() { let x = X {z,b,c:d} }",
        );
        super::SyntaxNode { coarse: true }.assert_all_selections(
            &buffer,
            Selection::default().set_range((CharIndex(23)..CharIndex(24)).into()),
            &[
                (22..23, "{"),
                (23..24, "z"),
                (24..25, ","),
                (25..26, "b"),
                (26..27, ","),
                (27..30, "c:d"),
                (30..31, "}"),
            ],
        );
    }

    #[test]
    fn vue_script_uses_injected_typescript_syntax_nodes() {
        let source = r#"<template><button>{{ count }}</button></template>
<script setup lang="ts">
const count = ref(1);
</script>
"#;

        assert_eq!(
            select_current_syntax_node_text(source, "count ="),
            "count = ref(1)"
        );
    }

    #[test]
    fn vue_style_uses_injected_scss_syntax_nodes() {
        let source = r#"<template><button class="button">{{ count }}</button></template>
<style lang="scss">
$theme-color: blue;
.button { color: $primary; }
</style>
"#;

        assert_eq!(
            select_current_syntax_node_text(source, "color: $"),
            "color: $primary;"
        );
    }

    #[test]
    fn vue_template_still_uses_host_syntax_nodes() {
        let source = r#"<template><button>{{ count }}</button></template>
<script setup lang="ts">
const count = ref(1);
</script>
"#;

        assert_eq!(select_current_syntax_node_text(source, "button"), "button");
    }

    #[test]
    fn case_2() {
        let buffer = Buffer::new(
            Some(tree_sitter_rust::LANGUAGE.into()),
            "fn main() { let x = S(a); }",
        );
        super::SyntaxNode { coarse: true }.assert_all_selections(
            &buffer,
            Selection::default().set_range((CharIndex(20)..CharIndex(21)).into()),
            &[(20..21, "S"), (21..24, "(a)")],
        );
    }

    #[test]
    fn parent() {
        let expected_parent: &str = "z.b";
        let buffer = Buffer::new(
            Some(tree_sitter_rust::LANGUAGE.into()),
            "fn main() { let x = z.b(); }",
        );

        let child_range = (CharIndex(20)..CharIndex(21)).into();

        let child_text = buffer.slice(&child_range).unwrap();
        assert_eq!(child_text, "z");
        let selection = super::SyntaxNode { coarse: false }.expand(&SelectionModeParams {
            buffer: &buffer,
            current_selection: &Selection::new(child_range),
            cursor_direction: &crate::components::editor::Direction::Start,
        });

        let parent_range = selection.unwrap().unwrap().selection.range();

        let parent_text = buffer.slice(&parent_range).unwrap();
        assert_eq!(parent_text, expected_parent);
    }

    #[test]
    fn first_child() {
        fn test(coarse: bool, expected_child: &str) {
            let buffer = Buffer::new(
                Some(tree_sitter_rust::LANGUAGE.into()),
                "fn main() { let x = {z}; }",
            );

            let parent_range = (CharIndex(20)..CharIndex(23)).into();

            let parent_text = buffer.slice(&parent_range).unwrap();
            assert_eq!(parent_text, "{z}");
            let selection = super::SyntaxNode { coarse }.down(
                &SelectionModeParams {
                    buffer: &buffer,
                    current_selection: &Selection::new(parent_range),
                    cursor_direction: &crate::components::editor::Direction::Start,
                },
                None,
            );

            let child_range = selection.unwrap().unwrap().selection.range();

            let child_text = buffer.slice(&child_range).unwrap();
            assert_eq!(child_text, expected_child);
        }
        test(true, "z");
        test(false, "{");
    }

    #[test]
    fn current_prioritize_same_line() {
        fn test(coarse: bool, expected_selection: &str) {
            let buffer = Buffer::new(
                Some(tree_sitter_rust::LANGUAGE.into()),
                "
fn main() {
  let x = X;
}"
                .trim(),
            );

            let range = (CharIndex(13)..CharIndex(17)).into();
            assert_eq!(buffer.slice(&range).unwrap(), " let");
            let selection = super::SyntaxNode { coarse }.current(
                &SelectionModeParams {
                    buffer: &buffer,
                    current_selection: &Selection::new(range),
                    cursor_direction: &crate::components::editor::Direction::Start,
                },
                IfCurrentNotFound::LookForward,
            );

            let actual_range = buffer.slice(&selection.unwrap().unwrap().range()).unwrap();
            assert_eq!(actual_range, expected_selection);
        }
        test(true, "let x = X;");
        test(false, "let");
    }

    #[serial]
    #[test]
    fn paste_forward_with_gap() -> anyhow::Result<()> {
        execute_test(|s| {
            Box::new([
                App(OpenFile {
                    path: s.main_rs(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                Editor(SetContent("fn f(x: X, y: Y) {}".to_string())),
                Editor(MatchLiteral("x: X".to_string())),
                Editor(SetSelectionMode(
                    IfCurrentNotFound::LookForward,
                    SelectionMode::SyntaxNode,
                )),
                Editor(MoveSelection(Right)),
                Editor(Copy),
                Editor(PasteWithMovement(GetGapMovement::Right)),
                Expect(CurrentComponentContent("fn f(x: X, y: Y, y: Y) {}")),
            ])
        })
    }

    #[serial]
    #[test]
    fn paste_backward_with_gap() -> anyhow::Result<()> {
        execute_test(|s| {
            Box::new([
                App(OpenFile {
                    path: s.main_rs(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                Editor(SetContent("fn f(x: X, y: Y) {}".to_string())),
                Editor(MatchLiteral("x: X".to_string())),
                Editor(SetSelectionMode(
                    IfCurrentNotFound::LookForward,
                    SelectionMode::SyntaxNode,
                )),
                Editor(MoveSelection(Right)),
                Editor(Copy),
                Editor(PasteWithMovement(GetGapMovement::Left)),
                Expect(CurrentComponentContent("fn f(x: X, y: Y, y: Y) {}")),
            ])
        })
    }

    #[serial]
    #[test]
    fn paste_indented_blocks_with_gaps() -> anyhow::Result<()> {
        execute_test(|s| {
            Box::new([
                App(OpenFile {
                    path: s.main_rs(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                Editor(SetContent(
                    "
mod x {
    fn main() {}

    fn foo() {}
}"
                    .to_string(),
                )),
                Editor(MatchLiteral("fn foo() {}".to_string())),
                Editor(SetSelectionMode(
                    IfCurrentNotFound::LookForward,
                    SelectionMode::SyntaxNode,
                )),
                Editor(Copy),
                Editor(PasteWithMovement(GetGapMovement::Right)),
                Expect(CurrentComponentContent(
                    "
mod x {
    fn main() {}

    fn foo() {}

    fn foo() {}
}",
                )),
            ])
        })
    }
}
