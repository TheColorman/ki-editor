use super::*;
use crate::{
    buffer::Buffer,
    components::editor::IfCurrentNotFound,
    selection::{CharIndex, Selection},
};

fn selected_node(source: &str, needle: &str) -> String {
    let language = crate::config::from_extension("vue").unwrap();
    let mut buffer = Buffer::new(language.tree_sitter_language(), source);
    buffer.set_language(language).unwrap();
    let start = source[..source.find(needle).unwrap()].chars().count();
    let selection = Selection::default().set_range((CharIndex(start)..CharIndex(start + 1)).into());
    let selected = SyntaxNode { coarse: true }
        .current(
            &super::super::SelectionModeParams {
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
fn vue_queries_select_script_style_and_host_nodes() {
    let source = r#"<template><button>{{ count }}</button></template>
<script setup lang="ts">const count = ref(1);</script>
<style lang="scss">.button { color: $primary; }</style>"#;
    assert_eq!(selected_node(source, "count ="), "count = ref(1)");
    assert_eq!(selected_node(source, "color: $"), "color: $primary;");
    assert_eq!(selected_node(source, "button"), "button");
}
