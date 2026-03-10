// Debug Go multiple return values
#[cfg(test)]
mod go_return_debug_tests {
    use crate::go::GoParser;
    use crate::traits::CodeIntelligence;

    #[test]
    fn debug_go_multiple_functions() {
        let source = b"func divide(a, b int) (int, error) {
    if b == 0 {
        return 0, errors.New(\"division by zero\")
    }
    return a / b, nil
}

func multiply(x, y int) int {
    return x * y
}";

        let parser = GoParser::new();
        let result = parser.get_signatures(source);

        println!("Result: {:?}", result);
        if let Ok(signatures) = result {
            println!("Found {} signatures", signatures.len());
            for sig in &signatures {
                println!("  - {}: {:?}", sig.name, sig);
            }
        }
    }
}
