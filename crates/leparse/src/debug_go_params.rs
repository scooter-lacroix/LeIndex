// Debug Go parameter extraction
#[cfg(test)]
mod go_param_debug_tests {
    use tree_sitter::Parser;

    #[test]
    fn debug_go_parameters_ast() {
        let source = b"func divide(a, b int) (int, error) {
    return a / b, nil
}";

        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_go::LANGUAGE.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        fn print_tree(node: &tree_sitter::Node, source: &[u8], depth: usize) {
            let indent = "  ".repeat(depth);
            let text = node.utf8_text(source).unwrap_or("<error>");
            let preview = if text.len() > 40 {
                format!("{}...", &text[..40])
            } else {
                text.to_string()
            };
            println!("{}[{}] '{}'", indent, node.kind(), preview);

            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                print_tree(&child, source, depth + 1);
            }
        }

        print_tree(&root, source, 0);
    }
}
