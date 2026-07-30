//! Shared CFG (control-flow graph) builder primitives for every language parser.
//!
//! The `CfgBuilder` struct and its four universal primitives (`new`,
//! `create_block`, `add_statement_to_block`, `finish`) are byte-identical across
//! all tree-sitter-backed parsers. Only the recursive kind-dispatch
//! (`build_cfg_recursive`) and the branch handlers (`handle_if_statement` /
//! `handle_loop_statement` / `handle_for_statement`) vary per language, so those
//! stay in each parser as an inherent `impl CfgBuilder` block on the
//! macro-generated local type.
//!
//! Defining the shared pieces via a macro (rather than a single shared struct)
//! keeps each parser's `CfgBuilder` a distinct per-module type — so the
//! language-specific methods don't collide as duplicate definitions — while the
//! duplicated method bodies live only here, eliminating the cross-parser clones.

/// Generate a local `CfgBuilder<'a>` struct plus its four universal primitives.
///
/// Invoke at module scope in each parser, before the parser-specific
/// `impl CfgBuilder` block. Requires `Block`, `Edge`, and `Graph` (from
/// `crate::parse::traits`) to be in scope at the invocation site.
#[macro_export]
macro_rules! cfg_builder {
    () => {
        struct CfgBuilder<'a> {
            source: &'a [u8],
            blocks: Vec<Block>,
            edges: Vec<Edge>,
            next_block_id: usize,
        }

        impl<'a> CfgBuilder<'a> {
            fn new(source: &'a [u8]) -> Self {
                Self {
                    source,
                    blocks: Vec::new(),
                    edges: Vec::new(),
                    next_block_id: 0,
                }
            }

            fn create_block(&mut self) -> usize {
                let id = self.next_block_id;
                self.next_block_id += 1;
                self.blocks.push(Block {
                    id,
                    statements: Vec::new(),
                });
                id
            }

            fn add_statement_to_block(&mut self, block_id: usize, statement: String) {
                if let Some(block) = self.blocks.get_mut(block_id) {
                    block.statements.push(statement);
                }
            }

            fn finish(self) -> Graph<Block, Edge> {
                Graph {
                    blocks: self.blocks,
                    edges: self.edges,
                    entry_block: 0,
                    exit_blocks: vec![self.next_block_id.saturating_sub(1)],
                }
            }

            /// Branch an if-statement into true/false/merge blocks with the
            /// standard four CFG edges. The node-kind dispatch is per-language;
            /// this edge topology is universal.
            fn handle_if_statement(
                &mut self,
                _node: &tree_sitter::Node<'_>,
                current_block: usize,
            ) -> Result<()> {
                let true_block = self.create_block();
                let false_block = self.create_block();
                let merge_block = self.create_block();
                self.edges.push(Edge {
                    from: current_block,
                    to: true_block,
                    edge_type: EdgeType::TrueBranch,
                });
                self.edges.push(Edge {
                    from: current_block,
                    to: false_block,
                    edge_type: EdgeType::FalseBranch,
                });
                self.edges.push(Edge {
                    from: true_block,
                    to: merge_block,
                    edge_type: EdgeType::Unconditional,
                });
                self.edges.push(Edge {
                    from: false_block,
                    to: merge_block,
                    edge_type: EdgeType::Unconditional,
                });
                Ok(())
            }
        }
    };
}
