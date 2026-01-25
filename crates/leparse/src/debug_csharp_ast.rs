/// Debug module to understand C# tree-sitter AST structure

use tree_sitter::Parser;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn debug_csharp_ast() {
        let source = b"public async Task<string> FetchData() {
    return await Task.FromResult(\"data\");
}";

        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_c_sharp::LANGUAGE.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        fn print_tree(node: &tree_sitter::Node, source: &[u8], indent: usize) {
            println!("{}{:?} {:?}",
                     " ".repeat(indent),
                     node.kind(),
                     node.utf8_text(source).unwrap_or(""));
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                print_tree(&child, source, indent + 2);
            }
        }

        print_tree(&root, source, 0);
    }
}
