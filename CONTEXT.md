# Resume Session Launcher

A terminal launcher that discovers local coding-agent sessions, presents enough context for a user to choose one, and resumes the selected session with its native agent CLI.

## Language

**Session**:
A resumable conversation or task state persisted by a coding agent.
_Avoid_: Chat, run

**Workspace**:
The filesystem directory recorded for a session and used as the working directory when resuming it. It may be a repository root, a Git worktree, or an ordinary directory.
_Avoid_: Project directory, current directory

**Resume**:
Start the session's native agent CLI in the current terminal with the Workspace as its working directory. Resume never selects a session automatically.
_Avoid_: Restore, reopen


**Unavailable Session**:
A discovered Session whose recorded Workspace no longer exists. It may be listed for diagnosis but cannot be resumed.
_Avoid_: Broken session, stale session

**Scope**:
The set of Sessions considered for listing. In Git, the default Scope covers every Workspace at any depth in the current worktree; `--all-worktrees` also includes linked worktrees. Outside Git, the default Scope contains only the current directory. An explicit direction expands from the current real directory by Directory Distance and overrides the Git default; it never scans unrelated sibling subtrees. There is no machine-wide Scope.
_Avoid_: Search range, project filter

**Directory Distance**:
The number of real path-component edges from the current directory used to expand Scope in exactly one direction. Distance 0 denotes only the current directory; upward and downward expansion are mutually exclusive. Either direction may use `all` instead of a finite distance.
_Avoid_: Ancestor level, recursion level, scan depth

**Session Preview**:
A read-only, presentation-normalized view of the user inputs persisted in a Session. It may collapse agent-injected skill instructions or markup noise, but never changes the underlying Session.
_Avoid_: Transcript editor, cleaned session

**Agent Integration**:
The independent capability to discover, preview, and resume Sessions persisted by one specific coding agent. The support of one agent neither requires nor implies support for another.
_Avoid_: Agent compatibility, universal session parser

**Support List**:
The declared set of coding agents whose Session behavior is tracked by the project, including their current support status. Installed agents are prioritized for validation, followed by agents with the broadest adoption.
_Avoid_: `npx skills` agent list, detected agents

**Support Status**:
The verified capability level of an Agent Integration: Supported, Discover Only, Unsupported, or Unavailable. Only Supported Sessions may be resumed. In v0.1.0, the current integrations can produce Supported, Discover Only, and Unavailable; Unsupported is modeled for a future integration that fails validation entirely and is not currently assigned.
_Avoid_: Compatibility flag, best-effort support

**Risk Status**:
A discovery-time signal that may require confirmation before Resume. In v0.1.0, integrations produce Normal or BroadWorkspace. WorkspaceChanged and ConflictingMetadata are reserved model variants and are not produced by current integration discovery; an actual workspace replacement is instead rejected during launch revalidation.
_Avoid_: Launch error, activity status

**Active Session**:
A Session that an Agent Integration can reliably associate with a currently running process. Its process and terminal details inform the user's Resume decision but do not determine it. In the v0.1.0 assembled app, integrations receive no live-correlation evidence, so discovery reports activity as Unknown by default; Active risk handling is implemented but has no live discovery trigger.
_Avoid_: Locked session, busy session

**Session Picker**:
The Skim-based terminal interface that incrementally receives Session candidates, performs fuzzy filtering and selection, and presents Session Preview in its preview pane.
_Avoid_: Custom TUI, session dashboard

**Agent Profile**:
A named isolation boundary within an agent for its authentication, settings, caches, and Sessions. When present, it is part of Session identity and must be preserved during Resume.
_Avoid_: Account, workspace

