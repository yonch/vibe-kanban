use rmcp::{
    ServerHandler,
    model::{Implementation, ProtocolVersion, ServerCapabilities, ServerInfo},
    tool_handler,
};

use super::{McpMode, McpServer};

#[tool_handler]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        let description = match self.mode() {
            McpMode::Global => {
                "A Vibe Kanban MCP server for task, issue, repository, workspace, and session management. Use list/read tools first when you need IDs or current state."
            }
            McpMode::Orchestrator => {
                "An orchestrator-scoped Vibe Kanban MCP server with tools limited to the configured workspace and orchestrator session context. Use list/read tools first when you need IDs or current state."
            }
        };

        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("vibe-kanban-mcp", "1.0.0")
                    .with_description(description),
            )
            .with_protocol_version(ProtocolVersion::V_2025_03_26)
    }
}
