use lazy_regex::regex;
use my_proc_macros::key;

use crate::{
    app::{Dimension, Dispatch::*},
    components::editor::{Direction, DispatchEditor::*},
    grid::{IndexedHighlightGroup, StyleKey},
    syntax_highlight::HighlightConfigs,
    test_app::{execute_test_custom, ExpectKind::*, RunTestOptions, Step::*},
};

fn assert_substring_highlighted_as(
    source_code: &str,
    highlighted_spans: &crate::syntax_highlight::HighlightedSpans,
    substring: &str,
    highlight_group: &str,
) {
    let start = source_code
        .find(substring)
        .unwrap_or_else(|| panic!("substring {substring:?} not found"));
    let end = start + substring.len();
    let expected_style =
        StyleKey::Syntax(IndexedHighlightGroup::from_str(highlight_group).unwrap());

    assert!(
        highlighted_spans.0.iter().any(|span| {
            span.style_key == expected_style
                && span.byte_range.start <= start
                && end <= span.byte_range.end
        }),
        "expected {substring:?} to be highlighted as {highlight_group:?}; spans: {:?}",
        highlighted_spans.0
    );
}

#[test]
fn syntax_highlight_json() -> anyhow::Result<()> {
    let options = RunTestOptions {
        enable_lsp: false,
        enable_syntax_highlighting: true,
        enable_file_watcher: false,
    };
    execute_test_custom(options, |s| {
        Box::new([
            App(AddPath(s.new_path("hello.json").display().to_string())),
            Expect(CurrentComponentTitle("File Explorer".to_string())),
            App(HandleKeyEvent(key!("enter"))),
            ExpectLater(Box::new(move || {
                CurrentComponentPath(Some(s.new_path("hello.json").try_into().unwrap()))
            })),
            Editor(SetContent(r#"{"x": 19}"#.to_string())),
            // Insert something to trigger syntax highlight request
            Editor(EnterInsertMode(Direction::End)),
            App(HandleKeyEvent(key!("space"))),
            WaitForAppMessage(regex!("SyntaxHighlightResponse")),
            App(TerminalDimensionChanged(Dimension {
                height: 20,
                width: 50,
            })),
            // Expect "x" is highlighted as "string"
            Expect(RangeStyleKey(
                "x",
                Some(StyleKey::Syntax(
                    IndexedHighlightGroup::from_str("string").unwrap(),
                )),
            )),
            // Expect 19 is highlighted as "number"
            Expect(RangeStyleKey(
                "19",
                Some(StyleKey::Syntax(
                    IndexedHighlightGroup::from_str("number").unwrap(),
                )),
            )),
        ])
    })
}

#[test]
fn syntax_highlight_vue_embedded_languages() -> anyhow::Result<()> {
    let source_code = r#"<template>
  <button :class="isActive ? 'active' : 'inactive'">{{ count + 1 }}</button>
</template>

<script setup lang="ts">
const count: number = 1;
</script>

<style>
.button { color: red; }
</style>

<style lang="scss">
$theme-color: blue;
.button { color: $primary; }
</style>
"#;

    let mut highlight_configs = HighlightConfigs::new();
    let highlighted_spans = highlight_configs.highlight(
        crate::config::from_extension("vue").unwrap(),
        source_code,
        &std::sync::atomic::AtomicUsize::new(0),
    )?;

    assert_substring_highlighted_as(source_code, &highlighted_spans, "button", "tag");
    assert_substring_highlighted_as(source_code, &highlighted_spans, "const", "keyword");
    assert_substring_highlighted_as(source_code, &highlighted_spans, "1", "number");
    assert_substring_highlighted_as(source_code, &highlighted_spans, "color", "property");
    assert_substring_highlighted_as(source_code, &highlighted_spans, "$primary", "variable");

    Ok(())
}
