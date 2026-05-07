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

use bash::BashToolConfig;

/// Create all default tools for the given working directory using the
/// default bash configuration (`/bin/bash`).
///
/// New call sites that need to honor `Settings.shell_path` should use
/// [`create_default_tools_with_config`] instead.
pub fn create_default_tools(cwd: &Path) -> Vec<AgentTool> {
    create_default_tools_with_config(cwd, BashToolConfig::default())
}

/// Create all default tools, threading a [`BashToolConfig`] into the bash
/// tool. Other tools are unaffected.
pub fn create_default_tools_with_config(
    cwd: &Path,
    bash_config: BashToolConfig,
) -> Vec<AgentTool> {
    let cwd = cwd.to_path_buf();
    vec![
        read::create_read_tool(cwd.clone()),
        write::create_write_tool(cwd.clone()),
        edit::create_edit_tool(cwd.clone()),
        bash::create_bash_tool_with_config(cwd.clone(), bash_config),
        grep::create_grep_tool(cwd.clone()),
        find::create_find_tool(cwd.clone()),
        ls::create_ls_tool(cwd),
    ]
}
