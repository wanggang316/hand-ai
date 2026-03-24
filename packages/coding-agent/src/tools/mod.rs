//! Built-in tools for the coding agent.
//!
//! Each tool is a factory function that returns an `AgentTool`.

pub mod bash;
pub mod edit;
pub mod find;
pub mod grep;
pub mod ls;
pub mod read;
pub mod write;

use hand_agent::types::AgentTool;
use std::path::Path;

/// Create all default tools for the given working directory.
pub fn create_default_tools(cwd: &Path) -> Vec<AgentTool> {
    let cwd = cwd.to_path_buf();
    vec![
        read::create_read_tool(cwd.clone()),
        write::create_write_tool(cwd.clone()),
        edit::create_edit_tool(cwd.clone()),
        bash::create_bash_tool(cwd.clone()),
        grep::create_grep_tool(cwd.clone()),
        find::create_find_tool(cwd.clone()),
        ls::create_ls_tool(cwd),
    ]
}
