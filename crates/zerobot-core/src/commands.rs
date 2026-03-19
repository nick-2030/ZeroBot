use std::path::Path;

pub fn init_prompt(cwd: &Path, extra: &str) -> String {
    let mut prompt = format!(
        "Please analyze this codebase and create an AGENTS.md file containing:\n\
1. Build/lint/test commands - especially for running a single test\n\
2. Code style guidelines including imports, formatting, types, naming conventions, error handling, etc.\n\n\
The file you create will be given to agentic coding agents (such as yourself) that operate in this repository. Make it about 150 lines long.\n\
If there are Cursor rules (in .cursor/rules/ or .cursorrules) or Copilot rules (in .github/copilot-instructions.md), make sure to include them.\n\n\
If there's already an AGENTS.md, improve it if it's located in {}\n",
        cwd.display()
    );
    let extra = extra.trim();
    if !extra.is_empty() {
        prompt.push_str("\n");
        prompt.push_str(extra);
        prompt.push_str("\n");
    }
    prompt
}
