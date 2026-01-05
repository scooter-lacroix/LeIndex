#!/usr/bin/env python3
import argparse
import json
import shutil

def install_claude_code():
    """
    Install/Configure for Claude Code.
    
    Since we are not on a marketplace, we provide instructions for MCP configuration.
    """
    print("\n--- Claude Code Configuration ---")
    print("To use LeIndex with Claude Code, you need to configure it as an MCP server.")
    print("\nAdd the following to your Claude Code configuration (usually config.json or via command line):")
    
    config = {
        "mcpServers": {
            "code-index": {
                "command": "code-index-mcp",
                "args": [],
                "env": {}
            }
        }
    }
    print(json.dumps(config, indent=2))
    
    # Check if we can automatically update a config file (heuristic)
    # Claude Code config location varies, so we stick to instructions for now unless we find a standard path.
    print("\nAdditionally, to use the 'code-search' skill effectively, ensure the CLI tool is in your PATH.")
    print(f"Current CLI path: {shutil.which('code-search') or 'Not found in PATH'}")

def install_openai():
    """
    Generate configuration for OpenAI Custom GPTs / Actions.
    """
    print("\n--- OpenAI Custom GPT / Actions Configuration ---")
    print("To integrate with OpenAI, you'll need to expose the MCP server via HTTP (e.g., using ngrok or a cloud deployment).")
    print("\n1. Start the server in HTTP mode:")
    print("   code-index-mcp --http --port 8080")
    print("\n2. Create an Action with the schema provided by the /mcp/schema endpoint.")

def install_vscode():
    """
    Generate configuration for VS Code (Claude Dev / Cline / etc).
    """
    print("\n--- VS Code (Claude Dev / Cline) Configuration ---")
    print("Add this to your VS Code settings or the extension's MCP configuration:")
    
    config = {
        "mcpServers": {
            "code-index": {
                "command": "uvx",
                "args": ["code-index-mcp"],
                "env": {},
                "disabled": False,
                "alwaysAllow": []
            }
        }
    }
    print(json.dumps(config, indent=2))

def main():
    parser = argparse.ArgumentParser(description="Install/Configure LeIndex for Agents")
    parser.add_argument("agent", choices=["claude-code", "openai", "vscode", "all"], help="The agent to configure")
    
    args = parser.parse_args()
    
    if args.agent == "claude-code" or args.agent == "all":
        install_claude_code()
    
    if args.agent == "openai" or args.agent == "all":
        install_openai()
        
    if args.agent == "vscode" or args.agent == "all":
        install_vscode()

if __name__ == "__main__":
    main()
