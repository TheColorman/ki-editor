use shared::absolute_path::AbsolutePath;

use crate::utils::normalize_line_endings_to_lf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IndentSettings {
    char: char,
    width: usize,
}

impl IndentSettings {
    pub(crate) fn new(char: char, width: usize) -> Self {
        Self { char, width }
    }

    pub(crate) fn char(self) -> char {
        self.char
    }

    pub(crate) fn width(self) -> usize {
        self.width
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct EditorConfigSettings {
    indent_char: Option<char>,
    indent_width: Option<usize>,
    insert_final_newline: Option<bool>,
    trim_trailing_whitespace: Option<bool>,
}

impl EditorConfigSettings {
    pub(crate) fn from_path(path: &AbsolutePath) -> Self {
        match editorconfig_core::properties(path.as_ref()) {
            Ok(properties) => Self::from_properties(&properties),
            Err(error) => {
                log::warn!("Failed to load EditorConfig for {path:?}: {error:?}");
                Self::default()
            }
        }
    }

    pub(crate) fn indent_settings(&self, fallback: IndentSettings) -> IndentSettings {
        IndentSettings::new(
            self.indent_char.unwrap_or(fallback.char()),
            self.indent_width.unwrap_or(fallback.width()),
        )
    }

    pub(crate) fn format_content_for_save(&self, content: &str) -> String {
        let mut content = normalize_line_endings_to_lf(content);

        if self.trim_trailing_whitespace == Some(true) {
            content = content
                .split('\n')
                .map(|line| line.trim_end_matches([' ', '\t']))
                .collect::<Vec<_>>()
                .join("\n");
        }

        if self.insert_final_newline == Some(true)
            && !content.is_empty()
            && !content.ends_with('\n')
        {
            content.push('\n');
        }

        content
    }

    fn from_properties(properties: &std::collections::HashMap<String, String>) -> Self {
        let indent_style = property(properties, "indent_style");
        let indent_size = property(properties, "indent_size");
        let insert_final_newline = property(properties, "insert_final_newline");
        let trim_trailing_whitespace = property(properties, "trim_trailing_whitespace");

        let indent_char = match indent_style {
            Some("tab") => Some('\t'),
            Some("space") => Some(' '),
            _ => None,
        };

        let indent_width = if indent_style == Some("tab") {
            Some(1)
        } else {
            indent_size.and_then(parse_width)
        };

        Self {
            indent_char,
            indent_width,
            insert_final_newline: insert_final_newline.and_then(parse_bool),
            trim_trailing_whitespace: trim_trailing_whitespace.and_then(parse_bool),
        }
    }
}

fn property<'a>(
    properties: &'a std::collections::HashMap<String, String>,
    key: &str,
) -> Option<&'a str> {
    properties
        .get(key)
        .map(String::as_str)
        .filter(|value| !value.eq_ignore_ascii_case("unset"))
}

fn parse_width(value: &str) -> Option<usize> {
    value.parse::<usize>().ok().filter(|width| *width > 0)
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{EditorConfigSettings, IndentSettings};

    fn config(properties: &[(&str, &str)]) -> EditorConfigSettings {
        EditorConfigSettings::from_properties(
            &properties
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect::<HashMap<_, _>>(),
        )
    }

    fn settings(properties: &[(&str, &str)]) -> IndentSettings {
        config(properties).indent_settings(IndentSettings::new('\t', 8))
    }

    #[test]
    fn parses_space_indentation() {
        assert_eq!(
            settings(&[("indent_style", "space"), ("indent_size", "2")]),
            IndentSettings::new(' ', 2)
        );
    }

    #[test]
    fn parses_tab_indentation_as_one_tab_per_level() {
        assert_eq!(
            settings(&[("indent_style", "tab"), ("indent_size", "4")]),
            IndentSettings::new('\t', 1)
        );
    }

    #[test]
    fn falls_back_per_property() {
        assert_eq!(
            settings(&[("indent_style", "space")]),
            IndentSettings::new(' ', 8)
        );
        assert_eq!(
            settings(&[("indent_size", "2")]),
            IndentSettings::new('\t', 2)
        );
    }

    #[test]
    fn ignores_unset_indentation_properties() {
        assert_eq!(
            settings(&[("indent_style", "unset"), ("indent_size", "unset")]),
            IndentSettings::new('\t', 8)
        );
    }

    #[test]
    fn trims_trailing_whitespace_on_save() {
        assert_eq!(
            config(&[("trim_trailing_whitespace", "true")]).format_content_for_save("a  \nb\t \n"),
            "a\nb\n"
        );
    }

    #[test]
    fn inserts_final_newline_on_save() {
        assert_eq!(
            config(&[("insert_final_newline", "true")]).format_content_for_save("a"),
            "a\n"
        );
    }

    #[test]
    fn ignores_crlf_line_endings_on_save() {
        assert_eq!(
            config(&[("end_of_line", "crlf")]).format_content_for_save("a\nb\n"),
            "a\nb\n"
        );
    }

    #[test]
    fn false_save_properties_still_normalize_line_endings() {
        assert_eq!(
            config(&[
                ("insert_final_newline", "false"),
                ("trim_trailing_whitespace", "false"),
            ])
            .format_content_for_save("a  \r\nb"),
            "a  \nb"
        );
    }
}
