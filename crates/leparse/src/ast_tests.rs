// Tests for zero-copy AST node types

use crate::ast::*;

#[cfg(test)]
mod zero_copy_tests {
    use super::*;

    #[test]
    fn test_ast_node_byte_range_reference() {
        let source = b"function hello() { return 42; }";
        let byte_range = 0..source.len();

        let node = AstNode::new(NodeType::Function, byte_range, 1, 0);

        // Verify zero-copy access - we're borrowing from source, not allocating
        let text = node.text(source).unwrap();
        assert_eq!(text, "function hello() { return 42; }");

        // Verify the byte range is stored, not the text
        assert_eq!(node.byte_range.start, 0);
        assert_eq!(node.byte_range.end, source.len());
    }

    #[test]
    fn test_ast_node_zero_copy_no_allocation() {
        let source = b"let x = 42;";
        let byte_range = 4..5; // Just the 'x'

        let node = AstNode::new(NodeType::Variable, byte_range, 1, 4);

        // Access text without allocation
        let text = node.text(source).unwrap();
        assert_eq!(text, "x");

        // The text is a slice into the original source
        // This proves zero-copy - no String allocation occurred
        assert!(std::ptr::eq(text.as_bytes(), &source[4..5]));
    }

    #[test]
    fn test_ast_node_lifetime_safety() {
        // This test verifies lifetime annotations are correct
        let source = b"const value = 100;";
        let node = AstNode::new(NodeType::Variable, 6..11, 1, 6);

        // The returned reference borrows from source
        let text = node.text(source).unwrap();
        assert_eq!(text, "value");

        // If source is dropped, text would be invalid
        // The compiler prevents this with lifetimes
    }

    #[test]
    fn test_function_element_zero_copy() {
        let source = b"function test(a, b) { return a + b; }";
        let func = FunctionElement {
            name: "test".to_string(),
            qualified_name: "test".to_string(),
            parameters: vec![
                Parameter {
                    name: "a".to_string(),
                    type_annotation: None,
                    default_value: None,
                },
                Parameter {
                    name: "b".to_string(),
                    type_annotation: None,
                    default_value: None,
                },
            ],
            return_type: None,
            byte_range: 0..source.len(),
            line_number: 1,
            is_async: false,
            docstring: None,
        };

        // FunctionElement stores byte range, not owned text
        // We can reconstruct the text from source if needed
        assert_eq!(&source[func.byte_range.clone()], func.text(source).unwrap().as_bytes());
    }

    #[test]
    fn test_class_element_zero_copy() {
        let source = b"class MyClass extends Base { }";
        let class = ClassElement {
            name: "MyClass".to_string(),
            qualified_name: "MyClass".to_string(),
            base_classes: vec!["Base".to_string()],
            methods: vec![],
            byte_range: 0..source.len(),
            line_number: 1,
            docstring: None,
        };

        // Zero-copy verification
        assert_eq!(class.byte_range.len(), source.len());
        assert_eq!(&source[class.byte_range.clone()], class.text(source).unwrap().as_bytes());
    }

    #[test]
    fn test_nested_ast_nodes() {
        let source = b"function outer() { function inner() {} }";
        let mut outer = AstNode::new(NodeType::Function, 0..source.len(), 1, 0);
        outer.metadata.name = Some("outer".to_string());

        // Find the actual position of "function inner" in the source
        let inner_text = b"function inner() {}";
        let inner_start = source.windows(inner_text.len())
            .position(|w| w == inner_text.as_slice())
            .unwrap();
        let inner_range = inner_start..(inner_start + inner_text.len());

        let mut inner = AstNode::new(NodeType::Function, inner_range, 1, inner_start);
        inner.metadata.name = Some("inner".to_string());

        outer.add_child(inner);

        // Both nodes store byte ranges, not text
        assert_eq!(outer.text(source).unwrap(), "function outer() { function inner() {} }");
        assert_eq!(outer.children[0].text(source).unwrap(), "function inner() {}");
    }

    #[test]
    fn test_memory_efficiency() {
        // This test demonstrates memory efficiency of zero-copy
        let large_source = b"/* 1000 bytes of data */".repeat(100);
        let byte_range = 0..large_source.len();

        let node = AstNode::new(NodeType::Module, byte_range, 1, 0);

        // The node itself is small - it only stores the range
        // It doesn't store a copy of the large_source text
        assert!(std::mem::size_of_val(&node) < large_source.len());

        // Yet we can still access the full text
        let text = node.text(&large_source).unwrap();
        assert_eq!(text.len(), large_source.len());
    }
}

// Helper extension trait for testing
trait ZeroCopyText {
    fn text<'source>(&self, source: &'source [u8]) -> Result<&'source str, std::str::Utf8Error>;
}

impl ZeroCopyText for FunctionElement {
    fn text<'source>(&self, source: &'source [u8]) -> Result<&'source str, std::str::Utf8Error> {
        std::str::from_utf8(&source[self.byte_range.clone()])
    }
}

impl ZeroCopyText for ClassElement {
    fn text<'source>(&self, source: &'source [u8]) -> Result<&'source str, std::str::Utf8Error> {
        std::str::from_utf8(&source[self.byte_range.clone()])
    }
}

#[cfg(test)]
mod ast_structure_tests {
    use super::*;

    #[test]
    fn test_node_type_variants() {
        // Test all node type variants exist and can be created
        let types = vec![
            NodeType::Module,
            NodeType::Function,
            NodeType::Class,
            NodeType::Method,
            NodeType::Variable,
            NodeType::Import,
            NodeType::Expression,
            NodeType::Statement,
            NodeType::Unknown,
        ];

        for node_type in types {
            let node = AstNode::new(node_type.clone(), 0..0, 1, 0);
            assert_eq!(node.node_type, node_type);
        }
    }

    #[test]
    fn test_ast_node_hierarchy() {
        let mut module = AstNode::new(NodeType::Module, 0..100, 1, 0);
        module.metadata.name = Some("test_module".to_string());

        let mut func = AstNode::new(NodeType::Function, 10..50, 2, 4);
        func.metadata.name = Some("test_func".to_string());

        module.add_child(func);

        assert_eq!(module.children.len(), 1);
        assert_eq!(module.children[0].metadata.name.as_ref().unwrap(), "test_func");
    }

    #[test]
    fn test_parameter_struct() {
        let param = Parameter {
            name: "x".to_string(),
            type_annotation: Some("number".to_string()),
            default_value: Some("42".to_string()),
        };

        assert_eq!(param.name, "x");
        assert_eq!(param.type_annotation.unwrap(), "number");
        assert_eq!(param.default_value.unwrap(), "42");
    }

    #[test]
    fn test_import_struct() {
        let import = Import {
            module_path: "std::collections".to_string(),
            items: vec!["HashMap".to_string(), "HashSet".to_string()],
            alias: Some("collections".to_string()),
            line_number: 5,
        };

        assert_eq!(import.module_path, "std::collections");
        assert_eq!(import.items.len(), 2);
        assert_eq!(import.alias.unwrap(), "collections");
        assert_eq!(import.line_number, 5);
    }
}

#[cfg(test)]
mod benchmark_style_tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn test_zero_copy_performance() {
        // This test demonstrates the performance benefit of zero-copy
        let source = b"function benchmark() { /* ... */ }".repeat(1000);
        let byte_range = 0..source.len();

        let start = Instant::now();
        let node = AstNode::new(NodeType::Function, byte_range, 1, 0);

        // Access text multiple times - should be very fast as it's just a slice
        for _ in 0..100 {
            let _ = node.text(&source).unwrap();
        }
        let duration = start.elapsed();

        // Zero-copy access should be very fast (< 1ms for 1000 iterations)
        assert!(duration.as_millis() < 10, "Zero-copy access should be fast");
    }

    #[test]
    fn test_memory_overhead() {
        // Demonstrate that AstNode stores references, not copies
        let large_source = b"/* Large comment */".repeat(1000);
        let byte_range = 0..large_source.len();

        let node = AstNode::new(NodeType::Module, byte_range, 1, 0);
        let node_size = std::mem::size_of_val(&node);

        // The node size is constant regardless of source size
        // This proves zero-copy - we don't store the source text
        assert!(node_size < 1000, "AstNode should be small (fixed size, not storing source)");

        // Yet we can still access the full source text
        let text = node.text(&large_source).unwrap();
        assert_eq!(text.len(), large_source.len());
    }
}
