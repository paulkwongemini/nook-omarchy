# Git account policy

All Git and GitHub actions for this repository must use the personal account `paulkwongemini`. This includes commit authorship and committer identity, authenticated clone/fetch/pull/push operations, and GitHub CLI/API operations.

- Required author and committer: `Paul <227434338+paulkwongemini@users.noreply.github.com>`.
- Use repository-local identity settings. Before creating or rewriting a commit, verify `git var GIT_AUTHOR_IDENT` and `git var GIT_COMMITTER_IDENT`, including environment overrides. Do not change the global Git identity.
- Verify remote authentication separately and explicitly select `paulkwongemini` credentials; do not assume the active CLI account, credential helper, or SSH key is correct. If the required account cannot be verified, stop rather than use another account. Never print or commit credentials.
