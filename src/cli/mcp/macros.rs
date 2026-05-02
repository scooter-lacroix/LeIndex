// Dispatch Macro for MCP Tool Handlers
//
// The `dispatch_handler!` macro generates the `ToolHandler` enum, its `Clone` derive,
// construction list (`all_tool_handlers`), and four dispatch methods (`name`, `description`,
// `argument_schema`, `execute`) from a single, compact declaration of (Variant, HandlerType)
// pairs.  Adding a new tool only requires appending one line to the invocation.

/// Declare the full MCP/CLI tool surface in one place.
///
/// # Usage
///
/// ```ignore
/// dispatch_handler! {
///     /// Doc comment on the enum variant (optional)
///     Index       => IndexHandler,
///     Search      => SearchHandler,
///     // … one line per tool
/// }
/// ```
///
/// This expands to:
/// - `#[derive(Clone)] pub enum ToolHandler { … }`
/// - `pub fn all_tool_handlers() -> Vec<ToolHandler>`
/// - `impl ToolHandler { pub fn name(&self) -> &str { … } }`
/// - `impl ToolHandler { pub fn description(&self) -> &str { … } }`
/// - `impl ToolHandler { pub fn argument_schema(&self) -> Value { … } }`
/// - `impl ToolHandler { pub async fn execute(…) -> Result<Value, JsonRpcError> { … } }`
#[macro_export]
macro_rules! dispatch_handler {
    (
        $(
            $(#[$meta:meta])*
            $variant:ident => $handler:ident
        ),* $(,)?
    ) => {
        /// Enum of all tool handlers.
        ///
        /// Instead of using trait objects (which don't work well with async),
        /// we use an enum to dispatch to the appropriate handler.
        #[derive(Clone)]
        pub enum ToolHandler {
            $(
                $(#[$meta])*
                $variant($handler),
            )*
        }

        /// Build the full MCP/CLI tool surface in one place so stdio, HTTP, and CLI bridges
        /// all stay in sync as new tools are added.
        pub fn all_tool_handlers() -> Vec<ToolHandler> {
            vec![
                $(
                    ToolHandler::$variant($handler),
                )*
            ]
        }

        impl ToolHandler {
            /// Get the tool name
            pub fn name(&self) -> &str {
                match self {
                    $(
                        ToolHandler::$variant(h) => h.name(),
                    )*
                }
            }

            /// Get the tool description
            pub fn description(&self) -> &str {
                match self {
                    $(
                        ToolHandler::$variant(h) => h.description(),
                    )*
                }
            }

            /// Get the tool argument schema
            pub fn argument_schema(&self) -> serde_json::Value {
                match self {
                    $(
                        ToolHandler::$variant(h) => h.argument_schema(),
                    )*
                }
            }

            /// Execute the tool
            pub async fn execute(
                &self,
                registry: &std::sync::Arc<crate::cli::registry::ProjectRegistry>,
                args: serde_json::Value,
            ) -> Result<serde_json::Value, $crate::cli::mcp::protocol::JsonRpcError> {
                match self {
                    $(
                        ToolHandler::$variant(h) => h.execute(registry, args).await,
                    )*
                }
            }
        }
    };
}
