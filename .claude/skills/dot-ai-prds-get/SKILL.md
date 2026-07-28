---
name: dot-ai-prds-get
description: "Fetch all open GitHub issues from this project that have the 'PRD' label"
user-invocable: true
---

# Get All PRDs

Fetch all open GitHub issues from this project that have the 'PRD' label.

**Note**: If any `gh` command fails with "command not found", inform the user that GitHub CLI is required and provide the installation link: https://cli.github.com/

## Process

1. **Fetch Issues**: Use GitHub CLI to get all open issues with PRD label
   ```bash
   gh issue list --label PRD --state open --limit 100 --json number,title,url,labels,assignees,createdAt,updatedAt,body
   ```

   `body` is required — dependencies, sequencing notes, and the PRD file link live there, and steps 2 and 4 below cannot be answered without it. `--limit 100` overrides `gh`'s default of 30.

2. **Fetch Declared Dependencies**: GitHub's native issue dependencies are not exposed by `gh issue list`. Query them per issue so the ordering is based on declared relationships rather than only on prose:
   ```bash
   gh api repos/{owner}/{repo}/issues/<n>/dependencies/blocked_by --jq '[.[].number]'
   gh api repos/{owner}/{repo}/issues/<n>/dependencies/blocking   --jq '[.[].number]'
   ```

   Treat these as authoritative when they disagree with prose in the body. Note that a closed blocker is no longer a blocker — check state before reporting an issue as blocked.

3. **Format Results**: Present the issues in a clear, organized format showing:
   - Issue number and title
   - Creation and last update dates  
   - Current assignees (if any)
   - Direct link to the issue
   - PRD file link (if available in issue description)

4. **Meaningful Categorization**: Group PRDs by their actual purpose and impact, not generic labels:
   - **Architecture & Infrastructure**: Core system changes, API designs, major refactors
   - **User Experience**: Features that directly impact how users interact with the system
   - **Developer Experience**: Tools, workflows, testing, documentation that help developers
   - **AI & Intelligence**: Machine learning, AI-powered features, recommendation engines
   - **Operations & Monitoring**: Deployment, scaling, observability, performance
   - **Integration & Extensibility**: Third-party integrations, plugin systems, APIs
   
   Each category should briefly explain what the PRDs in that group will accomplish for users or the system.

5. **Priority Analysis**: If multiple PRDs exist, help identify:
   - Which PRDs are most recently updated or have active discussion
   - Which PRDs have dependencies on other PRDs — from the step 2 `blocked_by`/`blocking` graph first, then from `**Depends on**` / `**Blocks**` / `**Sequencing**` lines in the body
   - Which PRDs are foundational vs. incremental improvements
   - Which PRDs might be blocked or need clarification

   Distinguish a **hard block** (declared `blocked_by`, or a body that says "do not start before X") from a **soft sequencing preference** (a body that says "X should land first" while both are independently startable). Only the former makes a PRD unavailable to pick up.

6. **Next Steps Suggestion**: Based on the PRD list, suggest logical next actions:
   - Which PRD to work on next based on dependencies and impact
   - PRDs that need attention, updates, or clarification
   - Opportunities for parallel work on independent PRDs

This provides a complete view of all active product requirements and helps with project planning and prioritization.

