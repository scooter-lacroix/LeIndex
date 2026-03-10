// Debug Go AST structure
#[cfg(test)]
mod go_debug_tests {
    use tree_sitter::Parser;

    #[test]
    fn debug_go_struct_ast() {
        let source = b"type Point struct {
    X float64
    Y float64
}";

        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        fn print_tree(node: &tree_sitter::Node, source: &[u8], depth: usize) {
            let indent = "  ".repeat(depth);
            let text = node.utf8_text(source).unwrap_or("<error>");
            let preview = if text.len() > 30 {
                format!("{}...", &text[..30])
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

    #[test]
    fn debug_go_interface_ast() {
        let source = b"type Writer interface {
    Write(p []byte) (n int, err error)
}";

        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        fn print_tree(node: &tree_sitter::Node, source: &[u8], depth: usize) {
            let indent = "  ".repeat(depth);
            let text = node.utf8_text(source).unwrap_or("<error>");
            let preview = if text.len() > 30 {
                format!("{}...", &text[..30])
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
