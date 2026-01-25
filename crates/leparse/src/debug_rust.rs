// Debug Rust AST structure
#[cfg(test)]
mod rust_debug_tests {
    use tree_sitter::Parser;

    #[test]
    fn debug_rust_function_ast() {
        let source = b"fn greet(name: &str) -> String {
    format!(\"Hello, {}\", name)
}";

        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
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

    #[test]
    fn debug_rust_async_function_ast() {
        let source = b"async fn fetch_data(url: &str) -> Result<String, Error> {
    Ok(String::new())
}";

        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
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

    #[test]
    fn debug_rust_visibility_ast() {
        let source = b"pub fn public_function() {}

fn private_function() {}

pub(crate) fn crate_function() {}";

        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
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
    fn debug_rust_self_parameters_ast() {
        let source = b"impl Foo {
    fn by_ref(&self) -> i32 { 0 }
    fn by_mut_ref(&mut self) -> i32 { 0 }
    fn by_value(self) -> i32 { 0 }
}";

        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
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
