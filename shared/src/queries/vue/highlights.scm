[
  (template_element)
  (tag_name)
  (start_tag)
  (end_tag)
] @tag

(erroneous_end_tag_name) @error
(attribute_name) @tag.attribute
(attribute_value) @property
(quoted_attribute_value) @string
(comment) @comment

(interpolation) @punctuation.special
(interpolation (raw_text) @none)

[
  (directive_modifier)
  (directive_name)
  (directive_value)
  (dynamic_directive_inner_value)
] @tag.attribute

"=" @operator
["<" ">" "</" "/>"] @tag.delimiter
