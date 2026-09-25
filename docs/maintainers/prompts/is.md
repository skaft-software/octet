---
description: Analyze a GitHub issue and propose a fix without implementing it
argument-hint: "<issue-number-or-url>"
---
Analyze this octet issue: ${@:-<issue-number-or-url>}

For each issue:

1. Read it in full, including every comment and any linked issues or pull
   requests:

   ```sh
   gh issue view <issue> --json title,body,comments,labels,assignees,state,url,author,createdAt,updatedAt,closedByPullRequestsReferences
   ```

2. Do not trust analysis written in the issue. Independently verify the behavior
   and derive your own analysis from the code and the execution path.

3. For bugs: ignore the issue's root-cause claim, read every related file in full
   (no truncation), trace the actual path, and name the true root cause plus the
   smallest fix that preserves the affected invariant.

4. For feature requests: do not treat the proposal as a design. Read the related
   code in full and propose the most concise implementation that fits octet's
   existing boundaries (host-owned policy, bounded input, no persisted trust
   default).

5. Report: affected files with paths, the proposed change, the regression test
   that would prove it, and the relevant user-facing documentation.

Do **not** implement unless explicitly asked. Analyze and propose only.
