You are analyzing issue {{issue}} of {{repo}}, in a git worktree of the repository. Do not implement it.

# {{title}}

{{body}}

{{latest}}

Read the relevant code and the repository's AGENTS.md or CLAUDE.md, then write an analysis for the people who will work on this issue:

- What the issue asks for, or what goes wrong and why, pointing at the code involved (files and functions).
- How you would solve it, and what the change would touch.
- How big the change is, and the risks or open questions.

Write the analysis in Markdown to the file `.amaru/analysis.md` in the repository root; Amaru posts it on the issue as a comment. Keep it short enough to read in a few minutes. Do not change, commit or push anything else, and do not comment on the issue yourself.
