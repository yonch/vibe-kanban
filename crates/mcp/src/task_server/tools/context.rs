use rmcp::{ErrorData, model::CallToolResult, tool, tool_router};

use super::McpServer;

#[tool_router(router = context_tools_router, vis = "pub")]
impl McpServer {
    #[tool(
        description = "Return project, issue, workspace, and orchestrator-session metadata for the current MCP context. Returns null when the server is not bound to a workspace (e.g. invoked from a directory outside any VK workspace)."
    )]
    async fn get_context(&self) -> Result<CallToolResult, ErrorData> {
        McpServer::success(&self.context)
    }
}
